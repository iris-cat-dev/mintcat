use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use mintcat_game_api::{
    canonical_digest, DeployPrecondition, LegacyFileDisposition, LegacyInstallReport,
    ValidatedGameInstallation,
};
use mintcat_library_service::{
    LibraryAuthorization, LibraryError, LibraryStore, ProfileContentGuard,
    CONTENT_WRITE_PERMISSION, READ_PERMISSION,
};
use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore};
use mintcat_runtime_file_transactions::{
    CommitDisposition, DeployFile, DeployRequest, ExpectedTarget, FileCancellation,
    FileTransactionError, FileTransactionManager, UninstallDisposition,
};
use mintcat_runtime_kernel::KernelDatabase;
use serde::Serialize;
use uuid::Uuid;

use crate::{
    profile::{capture_profile, verify_profile_snapshot},
    store::{InstallPlanStore, StoredPlanState},
    validation::{valid_digest, validate_build_product, verify_artifacts},
    AdapterBuildProduct, AdapterBuildRequest, ApplyDisposition, ApplyResult, ApprovalGrant,
    BuildPurpose, BuildRequest, BuildSummary, GameAdapterRegistry, ImmutableInstallPlan,
    InstallError, ModuleClosure, PackResult, PlanInstallRequest, ProfileBuildSnapshot,
    UnifiedUninstallDisposition, UnifiedUninstallResult, UninstallRequest,
};

const INSTALL_PLAN_SCHEMA_VERSION: u32 = 1;
const LEGACY_TAKEOVER_APPROVAL: &str = "legacy-install-takeover";

#[derive(Clone)]
pub struct InstallService {
    library: LibraryStore,
    authorization: LibraryAuthorization,
    owner_generation: String,
    artifacts: ArtifactStore,
    transactions: FileTransactionManager,
    plans: InstallPlanStore,
    adapters: GameAdapterRegistry,
}

struct ExecutedBuild {
    profile: ProfileBuildSnapshot,
    product: AdapterBuildProduct,
    summary: BuildSummary,
}

struct ContentGuardLease<'a> {
    library: &'a LibraryStore,
    guard: Option<ProfileContentGuard>,
}

impl ContentGuardLease<'_> {
    fn guard(&self) -> &ProfileContentGuard {
        self.guard.as_ref().expect("content guard is active")
    }

    fn release(mut self) -> Result<(), InstallError> {
        let guard = self.guard.take().expect("content guard is active");
        self.library.release_profile_content_guard(&guard)?;
        Ok(())
    }
}

impl Drop for ContentGuardLease<'_> {
    fn drop(&mut self) {
        if let Some(guard) = self.guard.take() {
            let _ = self.library.release_profile_content_guard(&guard);
        }
    }
}

impl InstallService {
    pub fn open(
        database: KernelDatabase,
        library: LibraryStore,
        owner_scope: impl Into<String>,
        owner_generation: impl Into<String>,
        artifacts: ArtifactStore,
        transactions: FileTransactionManager,
        adapters: GameAdapterRegistry,
    ) -> Result<Self, InstallError> {
        let owner_scope = owner_scope.into();
        if owner_scope.trim().is_empty() {
            return Err(InstallError::InvalidRequest(
                "install owner scope is required".to_owned(),
            ));
        }
        let owner_generation = owner_generation.into();
        if owner_generation.trim().is_empty() {
            return Err(InstallError::InvalidRequest(
                "install owner generation is required".to_owned(),
            ));
        }
        let plans = InstallPlanStore::open(database)?;
        let authorization =
            LibraryAuthorization::new(owner_scope, [READ_PERMISSION, CONTENT_WRITE_PERMISSION]);
        Ok(Self {
            library,
            authorization,
            owner_generation,
            artifacts,
            transactions,
            plans,
            adapters,
        })
    }

    pub fn pack(
        &self,
        request: BuildRequest,
        cancellation: &FileCancellation,
    ) -> Result<PackResult, InstallError> {
        let closure = request.module_closure.clone();
        let executed = self.execute_build(&request, BuildPurpose::Pack, cancellation)?;
        Ok(PackResult {
            profile: executed.profile,
            module_closure: closure,
            build_plan: executed.product.build_plan,
            outputs: executed.product.outputs,
            companion_manifests: executed.product.companion_manifests,
            summary: executed.summary,
        })
    }

    pub fn plan_install(
        &self,
        mut request: PlanInstallRequest,
        cancellation: &FileCancellation,
    ) -> Result<ImmutableInstallPlan, InstallError> {
        if request.expires_in_ms == 0 || request.expires_in_ms > 24 * 60 * 60 * 1_000 {
            return Err(InstallError::InvalidRequest(
                "install plan lifetime must be between 1 ms and 24 hours".to_owned(),
            ));
        }
        request
            .build
            .input_metadata
            .sort_by(|left, right| left.profile_mod_id.cmp(&right.profile_mod_id));
        let executed = self.execute_build(&request.build, BuildPurpose::Install, cancellation)?;
        if cancellation.is_requested() {
            return Err(InstallError::InvalidRequest(
                "install planning was cancelled before publication".to_owned(),
            ));
        }
        let adapter = self.adapters.get(
            &executed.profile.game_id, &request.build.module_closure.digest,
        )?;
        let root = Path::new(&executed.profile.root_path);
        let legacy_install = adapter
            .detect_legacy_install(root, &request.build.variant_id)
            .map_err(|message| adapter_error(&executed.profile.game_id, message))?;
        validate_legacy_report(
            &legacy_install,
            &executed.profile.game_id,
            &request.build.variant_id,
        )?;
        let deploy_plan = executed
            .product
            .deploy_plan
            .clone()
            .expect("install build validation requires a DeployPlan");
        let active_receipts = self
            .transactions
            .active_receipts_for_installation(&executed.profile.installation_id)?;
        validate_takeover_preconditions(&legacy_install, &deploy_plan, active_receipts.is_empty())?;
        let mut required_approvals = deploy_plan
            .files
            .iter()
            .flat_map(|file| file.approval_kinds.iter().cloned())
            .collect::<BTreeSet<_>>();
        if active_receipts.is_empty() && legacy_install.requires_approval {
            required_approvals.insert(LEGACY_TAKEOVER_APPROVAL.to_owned());
        }

        let created_at_ms = now_ms()?;
        let expires_at_ms = created_at_ms
            .checked_add(i64::try_from(request.expires_in_ms).map_err(|_| {
                InstallError::InvalidRequest("install plan lifetime overflows".to_owned())
            })?)
            .ok_or_else(|| {
                InstallError::InvalidRequest("install plan expiry overflows".to_owned())
            })?;
        let mut plan = ImmutableInstallPlan {
            schema_version: INSTALL_PLAN_SCHEMA_VERSION,
            plan_id: Uuid::new_v4().to_string(),
            plan_hash: String::new(),
            owner_scope: request.build.owner_scope,
            created_at_ms,
            expires_at_ms,
            variant_id: request.build.variant_id,
            profile: executed.profile,
            input_metadata: request.build.input_metadata,
            module_closure: request.build.module_closure,
            settings: request.build.settings,
            build_plan: executed.product.build_plan,
            deploy_plan,
            legacy_install,
            summary: executed.summary,
            required_approvals,
        };
        plan.plan_hash = install_plan_hash(&plan)?;
        verify_install_plan(&plan)?;
        self.plans.insert(&plan)?;
        Ok(plan)
    }

    pub fn apply(
        &self,
        plan_id: &str,
        plan_hash: &str,
        approval: &ApprovalGrant,
        cancellation: &FileCancellation,
    ) -> Result<ApplyResult, InstallError> {
        let stored = self.plans.get(plan_id)?;
        verify_install_plan(&stored.plan)?;
        if stored.plan.plan_hash != plan_hash {
            return Err(InstallError::Approval(
                "requested plan hash differs from the stored plan".to_owned(),
            ));
        }
        if stored.state == StoredPlanState::Applied {
            let receipt_id = stored.receipt_id.ok_or_else(|| InstallError::PlanState {
                plan_id: plan_id.to_owned(),
                state: "applied-without-receipt".to_owned(),
            })?;
            let receipt = self.transactions.receipt(&receipt_id)?;
            return Ok(ApplyResult {
                plan_id: plan_id.to_owned(),
                plan_hash: plan_hash.to_owned(),
                disposition: ApplyDisposition::AlreadyCommitted,
                receipt: Some(receipt),
                cancel_requested_during_commit: false,
            });
        }
        if stored.state == StoredPlanState::Applying {
            if let Some(receipt) = self.find_plan_receipt(&stored.plan)? {
                self.plans
                    .mark_applied(plan_id, &receipt.receipt_id, now_ms()?)?;
                return Ok(ApplyResult {
                    plan_id: plan_id.to_owned(),
                    plan_hash: plan_hash.to_owned(),
                    disposition: ApplyDisposition::AlreadyCommitted,
                    receipt: Some(receipt),
                    cancel_requested_during_commit: false,
                });
            }
            return Err(InstallError::PlanState {
                plan_id: plan_id.to_owned(),
                state: "applying-or-recovery-required".to_owned(),
            });
        }

        let now = now_ms()?;
        if now > stored.plan.expires_at_ms {
            return Err(InstallError::PlanExpired(plan_id.to_owned()));
        }
        validate_approval(&stored.plan, approval, now)?;
        if cancellation.is_requested() {
            return Ok(cancelled_apply(&stored.plan));
        }

        let guard = self.acquire_content_guard(&stored.plan)?;
        self.assert_profile_current(&stored.plan)?;
        self.library.validate_profile_content_guard(guard.guard())?;
        let adapter = self.adapters.get(
            &stored.plan.profile.game_id, &stored.plan.module_closure.digest,
        )?;
        let installation = adapter
            .validate_installation(
                Path::new(&stored.plan.profile.root_path),
                &stored.plan.variant_id,
            )
            .map_err(|message| adapter_error(&stored.plan.profile.game_id, message))?;
        validate_installation_identity(
            &stored.plan.profile,
            &installation,
            &stored.plan.variant_id,
        )?;
        if installation.fingerprint != stored.plan.build_plan.game_fingerprint {
            return Err(InstallError::InvalidRequest(
                "game installation fingerprint changed after planning".to_owned(),
            ));
        }
        self.assert_profile_current(&stored.plan)?;
        self.library.validate_profile_content_guard(guard.guard())?;
        if cancellation.is_requested() {
            guard.release()?;
            return Ok(cancelled_apply(&stored.plan));
        }

        let grants = stored
            .plan
            .deploy_plan
            .files
            .iter()
            .map(|file| self.artifacts.grant_blob(&file.artifact))
            .collect::<Result<Vec<_>, _>>()?;
        let files = deploy_files(&stored.plan, &grants);
        let guard_adapter = adapter.clone();
        let process_guard = move |root: &Path| guard_adapter.ensure_stopped(root);

        let begun = self.plans.begin_apply(plan_id, now_ms()?)?;
        if begun.state != StoredPlanState::Prepared {
            return Err(InstallError::PlanState {
                plan_id: plan_id.to_owned(),
                state: format!("{:?}", begun.state).to_ascii_lowercase(),
            });
        }
        let prepared = match self.transactions.prepare(
            DeployRequest {
                installation_id: stored.plan.profile.installation_id.clone(),
                owner_scope: stored.plan.owner_scope.clone(),
                root: stored.plan.profile.root_path.clone().into(),
                game_fingerprint: stored.plan.build_plan.game_fingerprint.clone(),
                profile_revision: stored.plan.profile.profile_revision,
                plan_hash: stored.plan.plan_hash.clone(),
                module_lock_digest: stored.plan.module_closure.digest.clone(),
                files,
            },
            &process_guard,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.plans.mark_prepared(plan_id, now_ms()?)?;
                return Err(error.into());
            }
        };
        let outcome = match self.transactions.commit(prepared, cancellation) {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(receipt) = self.find_plan_receipt(&stored.plan)? {
                    self.plans
                        .mark_applied(plan_id, &receipt.receipt_id, now_ms()?)?;
                } else if !matches!(error, FileTransactionError::RecoveryRequired { .. }) {
                    self.plans.mark_prepared(plan_id, now_ms()?)?;
                }
                return Err(error.into());
            }
        };
        let result = match outcome.disposition {
            CommitDisposition::Committed => {
                let receipt = outcome.receipt.ok_or_else(|| InstallError::PlanState {
                    plan_id: plan_id.to_owned(),
                    state: "committed-without-receipt".to_owned(),
                })?;
                self.plans
                    .mark_applied(plan_id, &receipt.receipt_id, now_ms()?)?;
                ApplyResult {
                    plan_id: plan_id.to_owned(),
                    plan_hash: plan_hash.to_owned(),
                    disposition: ApplyDisposition::Committed,
                    receipt: Some(receipt),
                    cancel_requested_during_commit: outcome.cancel_requested_during_commit,
                }
            }
            CommitDisposition::Cancelled => {
                self.plans.mark_prepared(plan_id, now_ms()?)?;
                ApplyResult {
                    plan_id: plan_id.to_owned(),
                    plan_hash: plan_hash.to_owned(),
                    disposition: ApplyDisposition::Cancelled,
                    receipt: None,
                    cancel_requested_during_commit: outcome.cancel_requested_during_commit,
                }
            }
        };
        guard.release()?;
        Ok(result)
    }

    pub fn uninstall(
        &self,
        request: &UninstallRequest,
        cancellation: &FileCancellation,
    ) -> Result<UnifiedUninstallResult, InstallError> {
        let active = self.uninstall_receipts(request)?;
        let newest = &active[0];
        let plan = self.plans.plan_for_receipt(&newest.receipt_id)?
            .ok_or_else(|| InstallError::PlanNotFound(newest.plan_hash.clone()))?;
        let adapter = self.adapters.get(&plan.profile.game_id, &plan.module_closure.digest)?;
        let process_guard = move |root: &Path| adapter.ensure_stopped(root);
        let receipt_ids = active.into_iter().map(|receipt| receipt.receipt_id).collect::<Vec<_>>();
        let report = self.transactions.uninstall_chain(&receipt_ids, &process_guard, cancellation)?;
        Ok(uninstall_result(request, report))
    }

    pub fn uninstall_receipt(
        &self,
        request: &UninstallRequest,
    ) -> Result<mintcat_runtime_file_transactions::InstallReceipt, InstallError> {
        Ok(self.uninstall_receipts(request)?.into_iter().next().expect("validated nonempty chain"))
    }

    fn uninstall_receipts(
        &self,
        request: &UninstallRequest,
    ) -> Result<Vec<mintcat_runtime_file_transactions::InstallReceipt>, InstallError> {
        let active = self.transactions.active_receipts_for_installation(&request.installation_id)?;
        let newest = active.first().ok_or_else(|| InstallError::InvalidRequest(
            format!("installation {} has no active receipt", request.installation_id),
        ))?;
        if active.iter().any(|receipt| receipt.owner_scope != self.authorization.owner_scope()) {
            return Err(InstallError::Approval("receipt chain belongs to another owner".to_owned()));
        }
        if newest.receipt_id != request.receipt_id {
            return Err(InstallError::InvalidRequest(format!(
                "receipt {} is not the newest active receipt for installation {}",
                request.receipt_id, request.installation_id,
            )));
        }
        if newest.profile_revision != request.expected_profile_revision {
            return Err(InstallError::ProfileChanged {
                expected_revision: request.expected_profile_revision,
                actual_revision: newest.profile_revision,
                expected_content_revision: 0,
                actual_content_revision: 0,
            });
        }
        Ok(active)
    }

    pub fn plan(&self, plan_id: &str) -> Result<ImmutableInstallPlan, InstallError> {
        let stored = self.plans.get(plan_id)?;
        verify_install_plan(&stored.plan)?;
        Ok(stored.plan)
    }

    pub fn recovery_build_request(&self, plan_id: &str) -> Result<BuildRequest, InstallError> {
        Ok(build_request_from_plan(self.plan(plan_id)?))
    }

    pub fn recovery_build_request_for_receipt(&self, receipt_id: &str) -> Result<BuildRequest, InstallError> {
        let plan = self.plans.plan_for_receipt(receipt_id)?
            .ok_or_else(|| InstallError::PlanNotFound(receipt_id.to_owned()))?;
        Ok(build_request_from_plan(plan))
    }

    pub fn committed_result(&self, plan_id: &str) -> Result<Option<ApplyResult>, InstallError> {
        let stored = self.plans.get(plan_id)?;
        verify_install_plan(&stored.plan)?;
        let receipt = if let Some(receipt_id) = &stored.receipt_id {
            Some(self.transactions.receipt(receipt_id)?)
        } else {
            self.find_plan_receipt(&stored.plan)?
        };
        let Some(receipt) = receipt else { return Ok(None); };
        self.plans.mark_applied(plan_id, &receipt.receipt_id, now_ms()?)?;
        Ok(Some(ApplyResult {
            plan_id: stored.plan.plan_id, plan_hash: stored.plan.plan_hash,
            disposition: ApplyDisposition::AlreadyCommitted, receipt: Some(receipt),
            cancel_requested_during_commit: false,
        }))
    }

    pub fn committed_uninstall_result(&self, request: &UninstallRequest) -> Result<Option<UnifiedUninstallResult>, InstallError> {
        Ok(self.transactions.committed_uninstall_result(&request.receipt_id)?
            .map(|report| uninstall_result(request, report)))
    }

    pub fn has_unresolved_plan(&self, plan_id: &str) -> Result<bool, InstallError> {
        self.plans.has_unresolved_plan(&self.plan(plan_id)?.plan_hash)
    }

    pub fn has_unresolved_uninstall(&self, installation_id: &str) -> Result<bool, InstallError> {
        Ok(self.transactions.has_unresolved_for_installation(installation_id)?)
    }
    pub fn has_unresolved_file_transactions(&self) -> Result<bool, InstallError> {
        Ok(self.transactions.has_unresolved()?)
    }


    pub fn recover_file_transactions(&self) -> Result<(), InstallError> {
        let plans = self.plans.plans()?;
        self.transactions.recover_incomplete(&|root: &Path| {
            let root = root.canonicalize().map_err(|error| error.to_string())?;
            let mut matched = false;
            for stored in &plans {
                let Ok(candidate) = Path::new(&stored.plan.profile.root_path).canonicalize() else { continue; };
                if candidate == root {
                    matched = true;
                    self.adapters.get(&stored.plan.profile.game_id, &stored.plan.module_closure.digest)
                        .map_err(|error| error.to_string())?.ensure_stopped(&root)?;
                }
            }
            if !matched { return Err("journal has no pinned install plan".to_owned()); }
            Ok(())
        })?;
        for stored in plans {
            if stored.state == StoredPlanState::Applying
                && self.committed_result(&stored.plan.plan_id)?.is_none()
                && !self.plans.has_unresolved_plan(&stored.plan.plan_hash)? {
                self.plans.mark_prepared(&stored.plan.plan_id, now_ms()?)?;
            }
        }
        Ok(())
    }

    pub fn release_recovered_content_guards(&self) -> Result<(), InstallError> {
        self.library.recover_profile_content_guards(
            &self.owner_generation, &self.plans.unresolved_profiles()?,
        )?;
        Ok(())
    }

    pub fn active_receipt_build_requests(&self) -> Result<Vec<BuildRequest>, InstallError> {
        self.plans.active_receipt_plans()?.into_iter().map(|plan| {
            verify_install_plan(&plan)?;
            Ok(build_request_from_plan(plan))
        }).collect()
    }

    pub fn retained_artifact_digests(&self, now_ms: i64) -> Result<BTreeSet<String>, InstallError> {
        let mut retained = BTreeSet::new();
        for plan in self.plans.retained_plans(now_ms)? {
            verify_install_plan(&plan)?;
            retained.extend(plan.module_closure.entries.into_iter().map(|entry| entry.package_digest));
            retained.extend(plan.profile.ordered_inputs.into_iter().map(|input| input.input.artifact.digest));
            retained.extend(plan.deploy_plan.files.into_iter().map(|file| file.artifact.digest));
        }
        Ok(retained)
    }

    fn execute_build(
        &self,
        request: &BuildRequest,
        purpose: BuildPurpose,
        cancellation: &FileCancellation,
    ) -> Result<ExecutedBuild, InstallError> {
        if request.owner_scope != self.authorization.owner_scope() {
            return Err(InstallError::InvalidRequest(
                "build owner scope differs from the install service owner".to_owned(),
            ));
        }
        request.settings.verify()?;
        request.module_closure.verify()?;
        if cancellation.is_requested() {
            return Err(InstallError::InvalidRequest(
                "build was cancelled before snapshot capture".to_owned(),
            ));
        }
        let profile = capture_profile(&self.library.snapshot()?, request)?;
        verify_profile_snapshot(&profile)?;
        let adapter = self.adapters.get(&profile.game_id, &request.module_closure.digest)?;
        let root = Path::new(&profile.root_path);
        adapter
            .ensure_stopped(root)
            .map_err(|message| adapter_error(&profile.game_id, message))?;
        let installation = adapter
            .validate_installation(root, &request.variant_id)
            .map_err(|message| adapter_error(&profile.game_id, message))?;
        validate_installation_identity(&profile, &installation, &request.variant_id)?;
        let grants = profile
            .ordered_inputs
            .iter()
            .map(|input| self.artifacts.grant_blob(&input.input.artifact))
            .collect::<Result<Vec<_>, _>>()?;
        let product = adapter
            .build(AdapterBuildRequest {
                purpose,
                installation: &installation,
                profile: &profile,
                settings: &request.settings,
                module_closure: &request.module_closure,
                input_grants: &grants,
                cancellation,
            })
            .map_err(|message| adapter_error(&profile.game_id, message))?;
        if cancellation.is_requested() {
            return Err(InstallError::InvalidRequest(
                "build was cancelled before output publication".to_owned(),
            ));
        }
        if product.build_plan.settings != request.settings
            || product.build_plan.variant_id != request.variant_id
            || product.build_plan.game_fingerprint != installation.fingerprint
        {
            return Err(InstallError::InvalidRequest(
                "game adapter BuildPlan changed settings, variant, or installation fingerprint"
                    .to_owned(),
            ));
        }
        let summary = validate_build_product(purpose, &profile, &request.module_closure, &product)?;
        let artifacts: Vec<_> = match purpose {
            BuildPurpose::Pack => product
                .outputs
                .iter()
                .map(|output| output.artifact.clone())
                .collect(),
            BuildPurpose::Install => product
                .deploy_plan
                .as_ref()
                .expect("validated install product has a DeployPlan")
                .files
                .iter()
                .map(|file| file.artifact.clone())
                .collect(),
        };
        verify_artifacts(&self.artifacts, artifacts)?;
        Ok(ExecutedBuild {
            profile,
            product,
            summary,
        })
    }

    fn acquire_content_guard(
        &self,
        plan: &ImmutableInstallPlan,
    ) -> Result<ContentGuardLease<'_>, InstallError> {
        let guard = self
            .library
            .acquire_profile_content_guard(
                &self.authorization,
                &format!("install-apply:{}", plan.plan_id),
                &plan.profile.profile_id,
                plan.profile.content_revision,
                &plan.profile.profile_incarnation_id,
                &self.owner_generation,
            )
            .map_err(|error| match error {
                LibraryError::RevisionConflict { expected, actual } => {
                    InstallError::ProfileChanged {
                        expected_revision: plan.profile.profile_revision,
                        actual_revision: plan.profile.profile_revision,
                        expected_content_revision: expected,
                        actual_content_revision: actual,
                    }
                }
                other => InstallError::Library(other),
            })?;
        Ok(ContentGuardLease {
            library: &self.library,
            guard: Some(guard),
        })
    }

    fn assert_profile_current(&self, plan: &ImmutableInstallPlan) -> Result<(), InstallError> {
        let current = capture_profile(
            &self.library.snapshot()?,
            &BuildRequest {
                owner_scope: plan.owner_scope.clone(),
                profile_id: plan.profile.profile_id.clone(),
                expected_profile_revision: plan.profile.profile_revision,
                expected_content_revision: plan.profile.content_revision,
                installation_id: plan.profile.installation_id.clone(),
                variant_id: plan.variant_id.clone(),
                settings: plan.settings.clone(),
                module_closure: plan.module_closure.clone(),
                input_metadata: plan.input_metadata.clone(),
            },
        )?;
        if current != plan.profile {
            return Err(InstallError::InvalidRequest(
                "Profile order, content closure, or installation changed after planning".to_owned(),
            ));
        }
        Ok(())
    }

    fn find_plan_receipt(
        &self,
        plan: &ImmutableInstallPlan,
    ) -> Result<Option<mintcat_runtime_file_transactions::InstallReceipt>, InstallError> {
        Ok(self
            .transactions
            .active_receipts_for_installation(&plan.profile.installation_id)?
            .into_iter()
            .find(|receipt| receipt.plan_hash == plan.plan_hash))
    }
}

fn deploy_files<'a>(
    plan: &'a ImmutableInstallPlan,
    grants: &'a [ArtifactGrant],
) -> Vec<DeployFile<'a>> {
    plan.deploy_plan
        .files
        .iter()
        .zip(grants)
        .map(|(file, grant)| DeployFile {
            relative_path: file.target_relative_path.clone(),
            artifact: grant,
            expected_target: match &file.precondition {
                DeployPrecondition::Absent => ExpectedTarget::Absent,
                DeployPrecondition::Digest(digest) => ExpectedTarget::Digest(digest.clone()),
            },
            owner: file.owner.clone(),
            install_mode: file.executable.then_some(0o755),
        })
        .collect()
}

fn validate_installation_identity(
    profile: &ProfileBuildSnapshot,
    installation: &ValidatedGameInstallation,
    variant_id: &str,
) -> Result<(), InstallError> {
    if installation.game_id != profile.game_id
        || installation.variant_id != variant_id
        || Path::new(&installation.root_path).canonicalize().map_err(|error| {
            InstallError::InvalidRequest(format!("validated root is unavailable: {error}"))
        })? != Path::new(&profile.root_path).canonicalize().map_err(|error| {
            InstallError::InvalidRequest(format!("selected root is unavailable: {error}"))
        })?
    {
        return Err(InstallError::InvalidRequest(
            "validated game installation differs from the selected Library installation".to_owned(),
        ));
    }
    Ok(())
}

fn validate_legacy_report(
    report: &LegacyInstallReport,
    game_id: &str,
    variant_id: &str,
) -> Result<(), InstallError> {
    if report.game_id != game_id || report.variant_id != variant_id {
        return Err(InstallError::InvalidRequest(
            "legacy-install report belongs to a different game installation".to_owned(),
        ));
    }
    let has_takeover = report
        .files
        .iter()
        .any(|file| file.disposition == LegacyFileDisposition::TakeoverRequired);
    if report.requires_approval != has_takeover {
        return Err(InstallError::InvalidRequest(
            "legacy-install report approval flag differs from its file dispositions".to_owned(),
        ));
    }
    Ok(())
}

fn validate_takeover_preconditions(
    legacy: &LegacyInstallReport,
    deploy: &mintcat_game_api::GameDeployPlan,
    has_no_receipt: bool,
) -> Result<(), InstallError> {
    if !has_no_receipt {
        return Ok(());
    }
    let deploy_by_path = deploy
        .files
        .iter()
        .map(|file| (file.target_relative_path.to_ascii_lowercase(), file))
        .collect::<BTreeMap<_, _>>();
    for candidate in &legacy.files {
        if candidate.disposition != LegacyFileDisposition::TakeoverRequired {
            continue;
        }
        let file = deploy_by_path
            .get(&candidate.relative_path.to_ascii_lowercase())
            .ok_or_else(|| {
                InstallError::InvalidRequest(format!(
                    "legacy takeover target {} is absent from the DeployPlan",
                    candidate.relative_path
                ))
            })?;
        if !matches!(&file.precondition, DeployPrecondition::Digest(digest) if valid_digest(digest))
        {
            return Err(InstallError::InvalidRequest(format!(
                "legacy takeover target {} is not bound to its current digest",
                candidate.relative_path
            )));
        }
    }
    Ok(())
}

fn validate_approval(
    plan: &ImmutableInstallPlan,
    approval: &ApprovalGrant,
    now_ms: i64,
) -> Result<(), InstallError> {
    if approval.approval_ref.trim().is_empty()
        || approval.plan_id != plan.plan_id
        || approval.plan_hash != plan.plan_hash
        || approval.expires_at_ms < now_ms
    {
        return Err(InstallError::Approval(
            "approval identity, hash, or expiry differs".to_owned(),
        ));
    }
    let missing = plan
        .required_approvals
        .difference(&approval.approved_risks)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(InstallError::Approval(format!(
            "missing risks: {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

fn verify_install_plan(plan: &ImmutableInstallPlan) -> Result<(), InstallError> {
    verify_profile_snapshot(&plan.profile)?;
    plan.module_closure.verify()?;
    plan.settings.verify()?;
    plan.deploy_plan.validate()?;
    if plan.schema_version != INSTALL_PLAN_SCHEMA_VERSION
        || plan.plan_id.trim().is_empty()
        || plan.owner_scope.trim().is_empty()
        || plan.expires_at_ms <= plan.created_at_ms
        || plan.plan_hash != install_plan_hash(plan)?
        || plan.build_plan.plan_hash != plan.deploy_plan.build_plan_hash
        || plan.profile.game_id != plan.build_plan.game_id
        || plan.profile.profile_revision != plan.build_plan.profile_revision
        || plan.module_closure.digest != plan.build_plan.module_lock_digest
    {
        return Err(InstallError::InvalidRequest(
            "immutable install plan identity or hash is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn install_plan_hash(plan: &ImmutableInstallPlan) -> Result<String, InstallError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HashableInstallPlan<'a> {
        schema_version: u32,
        plan_id: &'a str,
        owner_scope: &'a str,
        created_at_ms: i64,
        expires_at_ms: i64,
        variant_id: &'a str,
        profile: &'a ProfileBuildSnapshot,
        input_metadata: &'a [crate::ContentInputMetadata],
        module_closure: &'a ModuleClosure,
        settings: &'a mintcat_game_api::GameSettingsSnapshot,
        build_plan: &'a mintcat_game_api::GameBuildPlan,
        deploy_plan: &'a mintcat_game_api::GameDeployPlan,
        legacy_install: &'a LegacyInstallReport,
        summary: &'a BuildSummary,
        required_approvals: &'a BTreeSet<String>,
    }
    Ok(canonical_digest(&HashableInstallPlan {
        schema_version: plan.schema_version,
        plan_id: &plan.plan_id,
        owner_scope: &plan.owner_scope,
        created_at_ms: plan.created_at_ms,
        expires_at_ms: plan.expires_at_ms,
        variant_id: &plan.variant_id,
        profile: &plan.profile,
        input_metadata: &plan.input_metadata,
        module_closure: &plan.module_closure,
        settings: &plan.settings,
        build_plan: &plan.build_plan,
        deploy_plan: &plan.deploy_plan,
        legacy_install: &plan.legacy_install,
        summary: &plan.summary,
        required_approvals: &plan.required_approvals,
    })?)
}

fn build_request_from_plan(plan: ImmutableInstallPlan) -> BuildRequest {
    BuildRequest {
        owner_scope: plan.owner_scope, profile_id: plan.profile.profile_id,
        expected_profile_revision: plan.profile.profile_revision,
        expected_content_revision: plan.profile.content_revision,
        installation_id: plan.profile.installation_id, variant_id: plan.variant_id,
        settings: plan.settings, module_closure: plan.module_closure,
        input_metadata: plan.input_metadata,
    }
}

fn uninstall_result(
    request: &UninstallRequest,
    report: mintcat_runtime_file_transactions::UninstallChainReport,
) -> UnifiedUninstallResult {
    UnifiedUninstallResult {
        installation_id: request.installation_id.clone(), requested_receipt_id: request.receipt_id.clone(),
        disposition: match report.disposition {
            UninstallDisposition::Cancelled => UnifiedUninstallDisposition::Cancelled,
            UninstallDisposition::Completed => UnifiedUninstallDisposition::Completed,
            UninstallDisposition::CompletedWithPreservedFiles => UnifiedUninstallDisposition::CompletedWithPreservedFiles,
        },
        processed_receipt_ids: report.processed_receipt_ids, restored: report.restored, removed: report.removed,
        preserved_modified: report.preserved_modified, missing: report.missing,
        foreign_files_untouched: report.foreign_files_untouched,
        cancel_requested_during_commit: report.cancel_requested_during_commit,
    }
}

fn cancelled_apply(plan: &ImmutableInstallPlan) -> ApplyResult {
    ApplyResult {
        plan_id: plan.plan_id.clone(),
        plan_hash: plan.plan_hash.clone(),
        disposition: ApplyDisposition::Cancelled,
        receipt: None,
        cancel_requested_during_commit: false,
    }
}

fn adapter_error(game_id: &str, message: String) -> InstallError {
    InstallError::Adapter {
        game_id: game_id.to_owned(),
        message,
    }
}

pub(crate) fn now_ms() -> Result<i64, InstallError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            InstallError::InvalidRequest(format!("system clock is invalid: {error}"))
        })?;
    i64::try_from(duration.as_millis())
        .map_err(|_| InstallError::InvalidRequest("system clock overflows i64".to_owned()))
}
