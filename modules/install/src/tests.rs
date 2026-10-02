use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
};

use mintcat_engine_unreal_pak::{
    UnrealEngineLimits, UnrealInput, UnrealInputFormat, UnrealPakEngine,
};
use mintcat_game_api::{
    group_preconditions, BuiltGameOutput, DeployPrecondition, GameInputFormat, GameOutputRole,
    LegacyInstallReport,
};
use mintcat_game_drg::{
    create_build_plan as create_drg_build_plan, create_deploy_plan as create_drg_deploy_plan,
    detect_legacy_install as detect_drg_legacy, validate_installation as validate_drg,
    DrgBuildInput, DrgBuildLimits, DrgBuildPlanRequest, DrgBuildRequest, DrgBuilder, DrgSettings,
    DrgVariant, DRG_GAME_ID,
};
use mintcat_game_rogue_core::{
    create_build_plan as create_rc_build_plan, create_deploy_plan as create_rc_deploy_plan,
    detect_legacy_install as detect_rc_legacy, validate_installation as validate_rc,
    RogueCoreBuildInput, RogueCoreBuildLimits, RogueCoreBuildPlanRequest, RogueCoreBuildRequest,
    RogueCoreBuilder, RogueCoreSettings, RogueCoreVariant, ROGUE_CORE_GAME_ID,
};
use mintcat_integrator_core::uasset_utils::asset_registry::{AssetRegistry, Writable};
use mintcat_library_service::{
    ContentRefRecord, DownloadOperationRecord, GameDefinition, GameInstallation,
    LibraryAuthorization, LibraryBackup, LibraryMod, LibrarySnapshot, LibraryStore,
    ModContentVersion, ProfileContentBindingRecord, ProfileModRecord, ProfileRecord,
    SetProfileModEnabled, CONTENT_WRITE_PERMISSION, LIBRARY_BACKUP_FORMAT,
};
use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore, TrustStore};
use mintcat_runtime_file_transactions::{
    FileCancellation, FileTransactionError, FileTransactionManager,
};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::ArtifactRef;
use mintcat_unreal_pak::{PakBuildTarget, PakBundleWriter};
use sha2::{Digest as _, Sha256};
use tempfile::{tempdir, TempDir};

use super::*;

const OWNER: &str = "owner:test";
const PROFILE_REVISION: u64 = 7;
const CONTENT_REVISION: u64 = 3;

#[derive(Clone, Copy)]
enum FixtureGame {
    Drg,
    RogueCore,
}

impl FixtureGame {
    fn game_id(self) -> &'static str {
        match self {
            Self::Drg => DRG_GAME_ID,
            Self::RogueCore => ROGUE_CORE_GAME_ID,
        }
    }

    fn game_key(self) -> &'static str {
        match self {
            Self::Drg => "drg",
            Self::RogueCore => "rogue-core",
        }
    }

    fn variant_id(self) -> &'static str {
        match self {
            Self::Drg => "steam",
            Self::RogueCore => "steam-release",
        }
    }

    fn content_prefix(self) -> &'static str {
        match self {
            Self::Drg => "FSD",
            Self::RogueCore => "RogueCore",
        }
    }
}

#[derive(Default)]
struct AdapterControl {
    fail_build: AtomicBool,
    block_next_guard: AtomicBool,
    entered: (Mutex<bool>, Condvar),
    released: (Mutex<bool>, Condvar),
}

impl AdapterControl {
    fn guard(&self) -> Result<(), String> {
        if !self.block_next_guard.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let (entered, entered_cv) = &self.entered;
        *entered.lock().map_err(|_| "guard mutex poisoned")? = true;
        entered_cv.notify_all();
        let (released, released_cv) = &self.released;
        let mut released = released.lock().map_err(|_| "guard mutex poisoned")?;
        while !*released {
            released = released_cv
                .wait(released)
                .map_err(|_| "guard mutex poisoned")?;
        }
        Ok(())
    }

    fn block_next(&self) {
        *self.entered.0.lock().unwrap() = false;
        *self.released.0.lock().unwrap() = false;
        self.block_next_guard.store(true, Ordering::SeqCst);
    }

    fn wait_until_entered(&self) {
        let mut entered = self.entered.0.lock().unwrap();
        while !*entered {
            entered = self.entered.1.wait(entered).unwrap();
        }
    }

    fn release(&self) {
        *self.released.0.lock().unwrap() = true;
        self.released.1.notify_all();
    }
}

struct DrgTestAdapter {
    store: ArtifactStore,
    builder: DrgBuilder,
    engine: UnrealPakEngine,
    control: Arc<AdapterControl>,
}

impl GameInstallAdapter for DrgTestAdapter {
    fn game_id(&self) -> &str {
        DRG_GAME_ID
    }

    fn validate_installation(
        &self,
        root: &Path,
        variant_id: &str,
    ) -> Result<mintcat_game_api::ValidatedGameInstallation, String> {
        let variant = DrgVariant::from_id(variant_id).map_err(|error| error.to_string())?;
        validate_drg(root, variant).map_err(|error| error.to_string())
    }

    fn ensure_stopped(&self, _root: &Path) -> Result<(), String> {
        self.control.guard()
    }

    fn detect_legacy_install(
        &self,
        root: &Path,
        variant_id: &str,
    ) -> Result<LegacyInstallReport, String> {
        let variant = DrgVariant::from_id(variant_id).map_err(|error| error.to_string())?;
        detect_drg_legacy(root, variant).map_err(|error| error.to_string())
    }

    fn build(&self, request: AdapterBuildRequest<'_>) -> Result<AdapterBuildProduct, String> {
        if self.control.fail_build.load(Ordering::SeqCst) {
            return Err("injected build failure".to_owned());
        }
        let root = Path::new(&request.installation.root_path);
        let base_bytes = fs::read(root.join(&request.installation.main_pak_relative_path))
            .map_err(|error| error.to_string())?;
        let base = self
            .store
            .publish_blob_reader(
                &mut Cursor::new(base_bytes),
                Some("application/vnd.unreal.pak".to_owned()),
            )
            .map_err(|error| error.to_string())?;
        let base_grant = self
            .store
            .grant_blob(&base)
            .map_err(|error| error.to_string())?;
        let inputs = request.profile.planned_inputs();
        let pack_plan = create_drg_build_plan(DrgBuildPlanRequest {
            installation: request.installation.clone(),
            profile_revision: request.profile.profile_revision,
            module_lock_digest: request.module_closure.digest.clone(),
            settings: request.settings.clone(),
            base_game_artifact: base.clone(),
            patch_resource_lock_digest: None,
            ordered_inputs: inputs.clone(),
        })
        .map_err(|error| error.to_string())?;
        let grants = request
            .input_grants
            .iter()
            .zip(&inputs)
            .map(|(artifact, input)| DrgBuildInput {
                input_id: input.input_id.clone(),
                artifact,
            })
            .collect();
        let built = self
            .builder
            .build(DrgBuildRequest {
                plan: &pack_plan,
                base_game_artifact: &base_grant,
                inputs: grants,
                patch_package: None,
            })
            .map_err(|error| error.to_string())?;
        let conflicts = inspect_conflicts(&self.engine, &inputs, request.input_grants)?;
        if request.purpose == BuildPurpose::Pack {
            return Ok(AdapterBuildProduct {
                build_plan: pack_plan,
                outputs: built.outputs,
                deploy_plan: None,
                conflicts,
                companion_manifests: Vec::new(),
            });
        }

        let install_plan = create_drg_build_plan(DrgBuildPlanRequest {
            installation: request.installation.clone(),
            profile_revision: request.profile.profile_revision,
            module_lock_digest: request.module_closure.digest.clone(),
            settings: request.settings.clone(),
            base_game_artifact: base,
            patch_resource_lock_digest: Some(request.module_closure.digest.clone()),
            ordered_inputs: inputs,
        })
        .map_err(|error| error.to_string())?;
        let mut by_path = built
            .outputs
            .into_iter()
            .map(|output| (output.target_relative_path.to_ascii_lowercase(), output))
            .collect::<BTreeMap<_, _>>();
        let mut outputs = Vec::with_capacity(install_plan.outputs.len());
        for planned in &install_plan.outputs {
            if let Some(output) = by_path.remove(&planned.target_relative_path.to_ascii_lowercase())
            {
                outputs.push(output);
            } else if planned.role == GameOutputRole::HookDll {
                let artifact = self
                    .store
                    .publish_blob_reader(
                        &mut Cursor::new(b"MZ\x90\0signed-test-hook"),
                        Some("application/vnd.microsoft.portable-executable".to_owned()),
                    )
                    .map_err(|error| error.to_string())?;
                outputs.push(BuiltGameOutput {
                    role: planned.role.clone(),
                    target_relative_path: planned.target_relative_path.clone(),
                    artifact,
                    owner: planned.owner.clone(),
                    executable: planned.executable,
                });
            } else {
                return Err(format!(
                    "test adapter has no built output {}",
                    planned.target_relative_path
                ));
            }
        }
        let preconditions = output_preconditions(root, &outputs)?;
        let deploy_plan =
            create_drg_deploy_plan(&install_plan, outputs.clone(), Vec::new(), preconditions)
                .map_err(|error| error.to_string())?;
        Ok(AdapterBuildProduct {
            build_plan: install_plan,
            outputs,
            deploy_plan: Some(deploy_plan),
            conflicts,
            companion_manifests: Vec::new(),
        })
    }
}

struct RogueCoreTestAdapter {
    store: ArtifactStore,
    builder: RogueCoreBuilder,
    engine: UnrealPakEngine,
    control: Arc<AdapterControl>,
}

impl GameInstallAdapter for RogueCoreTestAdapter {
    fn game_id(&self) -> &str {
        ROGUE_CORE_GAME_ID
    }

    fn validate_installation(
        &self,
        root: &Path,
        variant_id: &str,
    ) -> Result<mintcat_game_api::ValidatedGameInstallation, String> {
        let variant = RogueCoreVariant::from_id(variant_id).map_err(|error| error.to_string())?;
        validate_rc(root, variant).map_err(|error| error.to_string())
    }

    fn ensure_stopped(&self, _root: &Path) -> Result<(), String> {
        self.control.guard()
    }

    fn detect_legacy_install(
        &self,
        root: &Path,
        variant_id: &str,
    ) -> Result<LegacyInstallReport, String> {
        let variant = RogueCoreVariant::from_id(variant_id).map_err(|error| error.to_string())?;
        detect_rc_legacy(root, variant).map_err(|error| error.to_string())
    }

    fn build(&self, request: AdapterBuildRequest<'_>) -> Result<AdapterBuildProduct, String> {
        if self.control.fail_build.load(Ordering::SeqCst) {
            return Err("injected build failure".to_owned());
        }
        let root = Path::new(&request.installation.root_path);
        let base_bytes = fs::read(root.join(&request.installation.main_pak_relative_path))
            .map_err(|error| error.to_string())?;
        let base = self
            .store
            .publish_blob_reader(
                &mut Cursor::new(base_bytes),
                Some("application/vnd.unreal.pak".to_owned()),
            )
            .map_err(|error| error.to_string())?;
        let base_grant = self
            .store
            .grant_blob(&base)
            .map_err(|error| error.to_string())?;
        let inputs = request.profile.planned_inputs();
        let pack_plan = create_rc_build_plan(RogueCoreBuildPlanRequest {
            installation: request.installation.clone(),
            profile_revision: request.profile.profile_revision,
            module_lock_digest: request.module_closure.digest.clone(),
            settings: request.settings.clone(),
            base_game_artifact: base.clone(),
            patch_resource_lock_digest: None,
            ordered_inputs: inputs.clone(),
        })
        .map_err(|error| error.to_string())?;
        let grants = request
            .input_grants
            .iter()
            .zip(&inputs)
            .map(|(artifact, input)| RogueCoreBuildInput {
                input_id: input.input_id.clone(),
                artifact,
            })
            .collect();
        let built = self
            .builder
            .build(RogueCoreBuildRequest {
                plan: &pack_plan,
                base_game_artifact: &base_grant,
                inputs: grants,
                patch_package: None,
            })
            .map_err(|error| error.to_string())?;
        let conflicts = inspect_conflicts(&self.engine, &inputs, request.input_grants)?;
        if request.purpose == BuildPurpose::Pack {
            return Ok(AdapterBuildProduct {
                build_plan: pack_plan,
                outputs: built.outputs,
                deploy_plan: None,
                conflicts,
                companion_manifests: Vec::new(),
            });
        }
        let install_plan = create_rc_build_plan(RogueCoreBuildPlanRequest {
            installation: request.installation.clone(),
            profile_revision: request.profile.profile_revision,
            module_lock_digest: request.module_closure.digest.clone(),
            settings: request.settings.clone(),
            base_game_artifact: base,
            patch_resource_lock_digest: Some(request.module_closure.digest.clone()),
            ordered_inputs: inputs,
        })
        .map_err(|error| error.to_string())?;
        let preconditions = output_preconditions(root, &built.outputs)?;
        let deploy_plan = create_rc_deploy_plan(
            &install_plan,
            built.outputs.clone(),
            Vec::new(),
            preconditions,
        )
        .map_err(|error| error.to_string())?;
        Ok(AdapterBuildProduct {
            build_plan: install_plan,
            outputs: built.outputs,
            deploy_plan: Some(deploy_plan),
            conflicts,
            companion_manifests: Vec::new(),
        })
    }
}

struct GameFixture {
    _sandbox: TempDir,
    root: PathBuf,
    library: LibraryStore,
    artifacts: ArtifactStore,
    service: InstallService,
    control: Arc<AdapterControl>,
    requests: Vec<BuildRequest>,
    output_path: String,
}

impl GameFixture {
    fn new(game: FixtureGame, profile_count: usize) -> Self {
        let sandbox = tempdir().unwrap();
        let layout = RuntimePaths::new(sandbox.path().join("runtime"))
            .unwrap()
            .initialize(RuntimeEnvironment::Test)
            .unwrap();
        let store = ArtifactStore::from_layout(
            &layout,
            RuntimeEnvironment::Test,
            "x86_64-pc-windows-msvc",
            "1",
            TrustStore::default(),
        )
        .unwrap();
        let root = sandbox.path().join("game");
        let descriptor = match game {
            FixtureGame::Drg => mintcat_game_drg::descriptor(),
            FixtureGame::RogueCore => mintcat_game_rogue_core::descriptor(),
        };
        let variant = descriptor
            .variants
            .iter()
            .find(|variant| variant.variant_id == game.variant_id())
            .unwrap();
        let main_path = root.join(&variant.main_pak_relative_path);
        fs::create_dir_all(main_path.parent().unwrap()).unwrap();
        fs::create_dir_all(root.join(&variant.binaries_relative_path)).unwrap();
        fs::write(
            &main_path,
            base_pak(&format!("{}/AssetRegistry.bin", game.content_prefix())),
        )
        .unwrap();

        let input_a = pak(&[
            (
                &format!("{}/Content/Mods/Shared/value.bin", game.content_prefix()),
                b"first".as_slice(),
            ),
            (
                &format!("{}/Content/Mods/First/only.bin", game.content_prefix()),
                b"one".as_slice(),
            ),
        ]);
        let input_b = pak(&[
            (
                &format!("{}/Content/Mods/Shared/value.bin", game.content_prefix()),
                b"second".as_slice(),
            ),
            (
                &format!("{}/Content/Mods/Second/only.bin", game.content_prefix()),
                b"two".as_slice(),
            ),
        ]);
        let artifact_a = publish(&store, &input_a);
        let artifact_b = publish(&store, &input_b);

        let library = LibraryStore::open(&layout).unwrap();
        let snapshot = library_snapshot(game, &root, &artifact_a, &artifact_b, profile_count);
        let backup_path = sandbox.path().join("library-backup.json");
        fs::write(
            &backup_path,
            serde_json::to_vec(&LibraryBackup {
                format: LIBRARY_BACKUP_FORMAT.to_owned(),
                exported_at_ms: 1,
                snapshot,
            })
            .unwrap(),
        )
        .unwrap();
        library.restore_from(&backup_path).unwrap();

        let control = Arc::new(AdapterControl::default());
        let engine = UnrealPakEngine::open(
            store.clone(),
            sandbox.path().join("conflicts"),
            UnrealEngineLimits::default(),
        )
        .unwrap();
        let adapter: Arc<dyn GameInstallAdapter> = match game {
            FixtureGame::Drg => Arc::new(DrgTestAdapter {
                store: store.clone(),
                builder: DrgBuilder::open(
                    store.clone(),
                    sandbox.path().join("drg-build"),
                    DrgBuildLimits::default(),
                )
                .unwrap(),
                engine,
                control: control.clone(),
            }),
            FixtureGame::RogueCore => Arc::new(RogueCoreTestAdapter {
                store: store.clone(),
                builder: RogueCoreBuilder::open(
                    store.clone(),
                    sandbox.path().join("rc-build"),
                    RogueCoreBuildLimits::default(),
                )
                .unwrap(),
                engine,
                control: control.clone(),
            }),
        };
        let database = KernelDatabase::open(&layout).unwrap();
        let transactions = FileTransactionManager::open(
            database.clone(),
            layout.state_dir.join("file-transactions"),
        )
        .unwrap();
        let closure = ModuleClosure::new(vec![ModuleClosureEntry {
            module_id: match game {
                FixtureGame::Drg => "mintcat.game.drg",
                FixtureGame::RogueCore => "mintcat.game.rogue-core",
            }
            .to_owned(),
            version: "1.0.0-test".to_owned(),
            package_digest: format!("sha256:{:064x}", 42),
        }])
        .unwrap();
        let service = InstallService::open(
            database,
            library.clone(),
            OWNER,
            "generation:test",
            store.clone(),
            transactions,
            GameAdapterRegistry::new([(closure.clone(), adapter)]).unwrap(),
        )
        .unwrap();
        let requests = (0..profile_count)
            .map(|index| BuildRequest {
                owner_scope: OWNER.to_owned(),
                profile_id: format!("profile:{index}"),
                expected_profile_revision: PROFILE_REVISION,
                expected_content_revision: CONTENT_REVISION,
                installation_id: format!("installation:{index}"),
                variant_id: game.variant_id().to_owned(),
                settings: match game {
                    FixtureGame::Drg => DrgSettings {
                        ue4ss_enabled: false,
                        ..DrgSettings::default()
                    }
                    .snapshot(1)
                    .unwrap(),
                    FixtureGame::RogueCore => RogueCoreSettings {
                        ue4ss_enabled: false,
                        ..RogueCoreSettings::default()
                    }
                    .snapshot(1)
                    .unwrap(),
                },
                module_closure: closure.clone(),
                input_metadata: vec![
                    ContentInputMetadata {
                        profile_mod_id: format!("profile-mod:{index}:a"),
                        format: GameInputFormat::Pak,
                        audio_only_hint: false,
                    },
                    ContentInputMetadata {
                        profile_mod_id: format!("profile-mod:{index}:b"),
                        format: GameInputFormat::Pak,
                        audio_only_hint: false,
                    },
                ],
            })
            .collect();
        Self {
            _sandbox: sandbox,
            root,
            library,
            artifacts: store,
            service,
            control,
            requests,
            output_path: variant.mod_pak_relative_path.clone(),
        }
    }

    fn request(&self, index: usize) -> BuildRequest {
        self.requests[index].clone()
    }

    fn plan(&self, index: usize) -> ImmutableInstallPlan {
        self.service
            .plan_install(
                PlanInstallRequest {
                    build: self.request(index),
                    expires_in_ms: 60_000,
                },
                &FileCancellation::default(),
            )
            .unwrap()
    }

    fn approval(plan: &ImmutableInstallPlan) -> ApprovalGrant {
        ApprovalGrant {
            approval_ref: format!("approval:{}", plan.plan_id),
            plan_id: plan.plan_id.clone(),
            plan_hash: plan.plan_hash.clone(),
            approved_risks: plan.required_approvals.clone(),
            expires_at_ms: plan.expires_at_ms,
        }
    }
}

fn library_snapshot(
    game: FixtureGame,
    root: &Path,
    artifact_a: &ArtifactRef,
    artifact_b: &ArtifactRef,
    profile_count: usize,
) -> LibrarySnapshot {
    let mut snapshot = LibrarySnapshot::default();
    snapshot.games.push(GameDefinition {
        game_id: game.game_id().to_owned(),
        game_key: game.game_key().to_owned(),
        display_name: game.game_id().to_owned(),
        icon: String::new(),
        active: true,
        created_at_ms: 1,
        updated_at_ms: 1,
    });
    for index in 0..profile_count {
        snapshot.installations.push(GameInstallation {
            installation_id: format!("installation:{index}"),
            game_id: game.game_id().to_owned(),
            root_path: Some(root.to_string_lossy().into_owned()),
            state: "configured".to_owned(),
            incarnation_id: format!("installation-incarnation:{index}"),
            created_at_ms: 1,
            updated_at_ms: 1,
        });
        snapshot.profiles.push(ProfileRecord {
            profile_id: format!("profile:{index}"),
            game_id: game.game_id().to_owned(),
            owner_scope: OWNER.to_owned(),
            name: format!("profile-{index}"),
            display_name: format!("Profile {index}"),
            active: index == 0,
            description: String::new(),
            revision: PROFILE_REVISION,
            content_revision: CONTENT_REVISION,
            lease_epoch: 0,
            incarnation_id: format!("profile-incarnation:{index}"),
            last_used_at_ms: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted_at_ms: None,
        });
        snapshot.download_operations.push(DownloadOperationRecord {
            operation_id: format!("download:{index}"),
            owner_scope: OWNER.to_owned(),
            idempotency_scope: "fixture".to_owned(),
            idempotency_key: format!("download:{index}"),
            inputs_hash: format!("sha256:{:064x}", index + 1),
            profile_id: format!("profile:{index}"),
            mode: "prepare-and-apply".to_owned(),
            expected_content_revision: CONTENT_REVISION,
            target_incarnation_id: format!("profile-incarnation:{index}"),
            owner_generation: "generation:fixture".to_owned(),
            fence_epoch: 1,
            state: "succeeded".to_owned(),
            result_json: Some("{}".to_owned()),
            error_code: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: Some(1),
        });
        for (suffix, artifact, order) in [("a", artifact_a, 10), ("b", artifact_b, 20)] {
            let mod_id = format!("mod:{index}:{suffix}");
            let profile_mod_id = format!("profile-mod:{index}:{suffix}");
            let version_id = format!("version:{index}:{suffix}");
            snapshot.mods.push(LibraryMod {
                mod_id: mod_id.clone(),
                game_id: game.game_id().to_owned(),
                source_id: "local".to_owned(),
                remote_game_key: game.game_key().to_owned(),
                remote_mod_id: format!("{index}-{}", if suffix == "a" { "1" } else { "2" }),
                revision: 1,
                incarnation_id: format!("mod-incarnation:{index}:{suffix}"),
                name_id: format!("mod-{suffix}"),
                display_name: format!("Mod {}", suffix.to_ascii_uppercase()),
                original_name: format!("Mod {suffix}"),
                url: String::new(),
                tags: Vec::new(),
                approval_status: "approved".to_owned(),
                dependency_mod_id: None,
                last_update_at_ms: 1,
                online_update_at_ms: 1,
                online_available: true,
                local_missing: false,
                created_at_ms: 1,
                updated_at_ms: 1,
            });
            snapshot.versions.push(ModContentVersion {
                mod_id: mod_id.clone(),
                version_id: version_id.clone(),
                remote_file_id: None,
                artifact_digest: Some(artifact.digest.clone()),
                legacy_location: None,
                availability: "remote".to_owned(),
                current: true,
                created_at_ms: 1,
                updated_at_ms: 1,
                source_url: None,
                file_size: artifact.size,
                legacy_download_status: None,
            });
            snapshot.profile_mods.push(ProfileModRecord {
                profile_mod_id: profile_mod_id.clone(),
                profile_id: format!("profile:{index}"),
                mod_id,
                group_id: None,
                incarnation_id: format!("profile-mod-incarnation:{index}:{suffix}"),
                sort_order: order,
                enabled: true,
                used_version: Some(version_id.clone()),
                version_selection_kind: "auto".to_owned(),
                selected_version_id: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            });
            snapshot
                .profile_content_bindings
                .push(ProfileContentBindingRecord {
                    profile_mod_id,
                    version_id,
                    artifact_digest: artifact.digest.clone(),
                    operation_id: format!("download:{index}"),
                    updated_at_ms: 1,
                });
        }
    }
    for (index, artifact) in [artifact_a, artifact_b].into_iter().enumerate() {
        snapshot.content_refs.push(ContentRefRecord {
            digest: artifact.digest.clone(),
            size: artifact.size,
            verified: true,
            grant_id: format!("grant:{index}"),
            created_at_ms: 1,
        });
    }
    snapshot
}

fn inspect_conflicts(
    engine: &UnrealPakEngine,
    inputs: &[mintcat_game_api::PlannedGameInput],
    grants: &[ArtifactGrant],
) -> Result<Vec<BuildConflict>, String> {
    let inputs = inputs
        .iter()
        .zip(grants)
        .map(|(input, artifact)| UnrealInput {
            input_id: input.input_id.clone(),
            artifact,
            format: match input.format {
                GameInputFormat::Pak => UnrealInputFormat::Pak,
                GameInputFormat::Zip => UnrealInputFormat::Zip,
                GameInputFormat::DirectorySnapshot => UnrealInputFormat::DirectorySnapshot,
            },
            content_prefix: None,
        })
        .collect();
    engine
        .conflicts(inputs)
        .map_err(|error| error.to_string())
        .map(|conflicts| {
            conflicts
                .into_iter()
                .map(|conflict| BuildConflict {
                    relative_path: conflict.relative_path,
                    winner_input_id: conflict.winner_input_id,
                    shadowed_input_ids: conflict.shadowed_input_ids,
                })
                .collect()
        })
}

fn output_preconditions(
    root: &Path,
    outputs: &[BuiltGameOutput],
) -> Result<BTreeMap<String, DeployPrecondition>, String> {
    group_preconditions(
        outputs
            .iter()
            .map(|output| {
                let path = root.join(&output.target_relative_path);
                let precondition = if path.is_file() {
                    let bytes = fs::read(&path).map_err(|error| error.to_string())?;
                    DeployPrecondition::Digest(format!("sha256:{:x}", Sha256::digest(bytes)))
                } else {
                    DeployPrecondition::Absent
                };
                Ok((output.target_relative_path.clone(), precondition))
            })
            .collect::<Result<Vec<_>, String>>()?,
    )
    .map_err(|error| error.to_string())
}

fn base_pak(registry_path: &str) -> Vec<u8> {
    let mut registry = Vec::new();
    AssetRegistry::empty().write(&mut registry).unwrap();
    pak(&[(registry_path, &registry)])
}

fn pak(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer =
        PakBundleWriter::new(Cursor::new(Vec::new()), &[], &PakBuildTarget::default()).unwrap();
    for (path, content) in files {
        writer.write_file(path, content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn publish(store: &ArtifactStore, bytes: &[u8]) -> ArtifactRef {
    store
        .publish_blob_reader(
            &mut Cursor::new(bytes),
            Some("application/vnd.unreal.pak".to_owned()),
        )
        .unwrap()
}

fn game_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, path: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn retained_install_content(fixture: &GameFixture, now_ms: i64) -> std::collections::HashSet<String> {
    let authorization = LibraryAuthorization::new(OWNER, [mintcat_library_service::READ_PERMISSION, CONTENT_WRITE_PERMISSION]);
    let mut retained: std::collections::HashSet<_> = fixture.library.retained_artifact_digests(&authorization).unwrap().into_iter().collect();
    retained.extend(fixture.service.retained_artifact_digests(now_ms).unwrap());
    retained
}

#[test]
fn prepared_install_artifacts_survive_gc_until_plan_expiry() {
    let fixture = GameFixture::new(FixtureGame::Drg, 1);
    let plan = fixture.plan(0);
    let executable = &plan.deploy_plan.files.iter().find(|file| file.executable).unwrap().artifact;
    let valid_gc = fixture.artifacts.collect_garbage(&retained_install_content(&fixture, plan.expires_at_ms)).unwrap();
    assert!(valid_gc.retained.contains(&format!("blob:{}", executable.digest)));
    let expired_gc = fixture.artifacts.collect_garbage(&retained_install_content(&fixture, plan.expires_at_ms + 1)).unwrap();
    assert!(expired_gc.removed.contains(&format!("blob:{}", executable.digest)));
}

#[test]
fn active_receipt_artifacts_survive_expiry_and_release_after_uninstall() {
    let fixture = GameFixture::new(FixtureGame::Drg, 1);
    let plan = fixture.plan(0);
    let executable = &plan.deploy_plan.files.iter().find(|file| file.executable).unwrap().artifact;
    let applied = fixture.service.apply(&plan.plan_id, &plan.plan_hash, &GameFixture::approval(&plan), &FileCancellation::default()).unwrap();
    let receipt = applied.receipt.unwrap();
    let active_gc = fixture.artifacts.collect_garbage(&retained_install_content(&fixture, plan.expires_at_ms + 1)).unwrap();
    assert!(active_gc.retained.contains(&format!("blob:{}", executable.digest)));
    let result = fixture.service.uninstall(&UninstallRequest {
        installation_id: "installation:0".to_owned(),
        receipt_id: receipt.receipt_id,
        expected_profile_revision: PROFILE_REVISION,
    }, &FileCancellation::default()).unwrap();
    assert_eq!(result.disposition, UnifiedUninstallDisposition::Completed);
    let released_gc = fixture.artifacts.collect_garbage(&retained_install_content(&fixture, plan.expires_at_ms + 1)).unwrap();
    assert!(released_gc.removed.contains(&format!("blob:{}", executable.digest)));
}

#[test]
fn drg_pack_install_and_uninstall_run_without_a_gui_and_report_first_wins() {
    let fixture = GameFixture::new(FixtureGame::Drg, 1);
    let before_pack = game_files(&fixture.root);
    let pack = fixture
        .service
        .pack(fixture.request(0), &FileCancellation::default())
        .unwrap();
    assert_eq!(game_files(&fixture.root), before_pack);
    assert_eq!(pack.summary.winners.len(), 1);
    assert_eq!(pack.summary.winners[0].winner_display_name, "Mod A");
    assert_eq!(pack.summary.winners[0].shadowed_display_names, ["Mod B"]);
    assert_eq!(pack.summary.winners[0].rule, "first-input-wins");

    let plan = fixture.plan(0);
    assert!(plan.required_approvals.contains("native-code-execution"));
    let applied = fixture
        .service
        .apply(
            &plan.plan_id,
            &plan.plan_hash,
            &GameFixture::approval(&plan),
            &FileCancellation::default(),
        )
        .unwrap();
    let receipt = applied.receipt.unwrap();
    assert!(fixture.root.join(&fixture.output_path).is_file());
    assert_eq!(receipt.plan_hash, plan.plan_hash);

    let replacement_plan = fixture.plan(0);
    let replacement_receipt = fixture
        .service
        .apply(
            &replacement_plan.plan_id,
            &replacement_plan.plan_hash,
            &GameFixture::approval(&replacement_plan),
            &FileCancellation::default(),
        )
        .unwrap()
        .receipt
        .unwrap();
    assert_ne!(replacement_receipt.receipt_id, receipt.receipt_id);

    let uninstalled = fixture
        .service
        .uninstall(
            &UninstallRequest {
                installation_id: "installation:0".to_owned(),
                receipt_id: replacement_receipt.receipt_id,
                expected_profile_revision: PROFILE_REVISION,
            },
            &FileCancellation::default(),
        )
        .unwrap();
    assert_eq!(
        uninstalled.disposition,
        UnifiedUninstallDisposition::Completed
    );
    assert_eq!(uninstalled.processed_receipt_ids.len(), 2);
    assert!(!fixture.root.join(&fixture.output_path).exists());
    assert!(uninstalled.foreign_files_untouched);
}

#[test]
fn rogue_core_pack_install_and_uninstall_run_without_a_gui() {
    let fixture = GameFixture::new(FixtureGame::RogueCore, 1);
    let before_pack = game_files(&fixture.root);
    let pack = fixture
        .service
        .pack(fixture.request(0), &FileCancellation::default())
        .unwrap();
    assert_eq!(game_files(&fixture.root), before_pack);
    assert_eq!(pack.build_plan.game_id, ROGUE_CORE_GAME_ID);
    assert_eq!(pack.summary.winners[0].winner_input_id, "profile-mod:0:a");

    let plan = fixture.plan(0);
    let applied = fixture
        .service
        .apply(
            &plan.plan_id,
            &plan.plan_hash,
            &GameFixture::approval(&plan),
            &FileCancellation::default(),
        )
        .unwrap();
    let receipt = applied.receipt.unwrap();
    assert!(fixture.root.join(&fixture.output_path).is_file());
    let uninstalled = fixture
        .service
        .uninstall(
            &UninstallRequest {
                installation_id: "installation:0".to_owned(),
                receipt_id: receipt.receipt_id,
                expected_profile_revision: PROFILE_REVISION,
            },
            &FileCancellation::default(),
        )
        .unwrap();
    assert_eq!(
        uninstalled.disposition,
        UnifiedUninstallDisposition::Completed
    );
    assert!(!fixture.root.join(&fixture.output_path).exists());
}

#[test]
fn legacy_takeover_requires_bound_approval_and_uninstall_restores_backup() {
    let fixture = GameFixture::new(FixtureGame::RogueCore, 1);
    let old = b"legacy managed output";
    let target = fixture.root.join(&fixture.output_path);
    fs::write(&target, old).unwrap();
    let plan = fixture.plan(0);
    assert!(plan.required_approvals.contains("legacy-install-takeover"));
    let mut incomplete = GameFixture::approval(&plan);
    incomplete.approved_risks.remove("legacy-install-takeover");
    assert!(matches!(
        fixture.service.apply(
            &plan.plan_id,
            &plan.plan_hash,
            &incomplete,
            &FileCancellation::default()
        ),
        Err(InstallError::Approval(_))
    ));
    assert_eq!(fs::read(&target).unwrap(), old);

    let receipt = fixture
        .service
        .apply(
            &plan.plan_id,
            &plan.plan_hash,
            &GameFixture::approval(&plan),
            &FileCancellation::default(),
        )
        .unwrap()
        .receipt
        .unwrap();
    assert_ne!(fs::read(&target).unwrap(), old);
    fixture
        .service
        .uninstall(
            &UninstallRequest {
                installation_id: "installation:0".to_owned(),
                receipt_id: receipt.receipt_id,
                expected_profile_revision: PROFILE_REVISION,
            },
            &FileCancellation::default(),
        )
        .unwrap();
    assert_eq!(fs::read(target).unwrap(), old);
}

#[test]
fn cancellation_profile_change_target_change_and_build_failure_preserve_old_install() {
    let fixture = GameFixture::new(FixtureGame::RogueCore, 1);
    let target = fixture.root.join(&fixture.output_path);
    fs::write(&target, b"old-before-cancel").unwrap();
    let cancel_plan = fixture.plan(0);
    let cancellation = FileCancellation::default();
    cancellation.request();
    let cancelled = fixture
        .service
        .apply(
            &cancel_plan.plan_id,
            &cancel_plan.plan_hash,
            &GameFixture::approval(&cancel_plan),
            &cancellation,
        )
        .unwrap();
    assert_eq!(cancelled.disposition, ApplyDisposition::Cancelled);
    assert_eq!(fs::read(&target).unwrap(), b"old-before-cancel");

    let stale_plan = fixture.plan(0);
    let authorization = LibraryAuthorization::new(OWNER, [CONTENT_WRITE_PERMISSION]);
    fixture
        .library
        .set_profile_mod_enabled(
            &authorization,
            &SetProfileModEnabled {
                idempotency_key: "disable-after-plan".to_owned(),
                profile_id: "profile:0".to_owned(),
                profile_mod_id: "profile-mod:0:b".to_owned(),
                expected_content_revision: CONTENT_REVISION,
                enabled: false,
            },
        )
        .unwrap();
    assert!(matches!(
        fixture.service.apply(
            &stale_plan.plan_id,
            &stale_plan.plan_hash,
            &GameFixture::approval(&stale_plan),
            &FileCancellation::default()
        ),
        Err(InstallError::ProfileChanged { .. })
    ));
    assert_eq!(fs::read(&target).unwrap(), b"old-before-cancel");

    let target_fixture = GameFixture::new(FixtureGame::RogueCore, 1);
    let target = target_fixture.root.join(&target_fixture.output_path);
    fs::write(&target, b"planned-old").unwrap();
    let plan = target_fixture.plan(0);
    fs::write(&target, b"externally-changed").unwrap();
    assert!(matches!(
        target_fixture.service.apply(
            &plan.plan_id,
            &plan.plan_hash,
            &GameFixture::approval(&plan),
            &FileCancellation::default()
        ),
        Err(InstallError::FileTransaction(
            FileTransactionError::TargetMismatch { .. }
        ))
    ));
    assert_eq!(fs::read(&target).unwrap(), b"externally-changed");

    let failed_fixture = GameFixture::new(FixtureGame::Drg, 1);
    let target = failed_fixture.root.join(&failed_fixture.output_path);
    fs::write(&target, b"old-before-build-failure").unwrap();
    failed_fixture
        .control
        .fail_build
        .store(true, Ordering::SeqCst);
    assert!(matches!(
        failed_fixture.service.plan_install(
            PlanInstallRequest {
                build: failed_fixture.request(0),
                expires_in_ms: 60_000,
            },
            &FileCancellation::default()
        ),
        Err(InstallError::Adapter { .. })
    ));
    assert_eq!(fs::read(target).unwrap(), b"old-before-build-failure");
}

#[test]
fn backend_owned_apply_survives_client_drop_and_replays_explicit_result() {
    let fixture = GameFixture::new(FixtureGame::RogueCore, 1);
    let plan = fixture.plan(0);
    let service = fixture.service.clone();
    let plan_for_job = plan.clone();
    let job = thread::spawn(move || {
        service.apply(
            &plan_for_job.plan_id,
            &plan_for_job.plan_hash,
            &GameFixture::approval(&plan_for_job),
            &FileCancellation::default(),
        )
    });
    drop(plan);
    let applied = job.join().unwrap().unwrap();
    let receipt = applied.receipt.unwrap();
    let replay = fixture
        .service
        .apply(
            &applied.plan_id,
            &applied.plan_hash,
            &ApprovalGrant {
                approval_ref: "replay".to_owned(),
                plan_id: applied.plan_id.clone(),
                plan_hash: applied.plan_hash.clone(),
                approved_risks: BTreeSet::new(),
                expires_at_ms: i64::MAX,
            },
            &FileCancellation::default(),
        )
        .unwrap();
    assert_eq!(replay.disposition, ApplyDisposition::AlreadyCommitted);
    assert_eq!(replay.receipt.unwrap().receipt_id, receipt.receipt_id);
}

#[test]
fn same_physical_game_directory_is_mutually_exclusive_across_installations() {
    let fixture = GameFixture::new(FixtureGame::RogueCore, 2);
    let first = fixture.plan(0);
    let second = fixture.plan(1);
    fixture.control.block_next();
    let service = fixture.service.clone();
    let first_for_job = first.clone();
    let first_job = thread::spawn(move || {
        service.apply(
            &first_for_job.plan_id,
            &first_for_job.plan_hash,
            &GameFixture::approval(&first_for_job),
            &FileCancellation::default(),
        )
    });
    fixture.control.wait_until_entered();
    let second_result = fixture.service.apply(
        &second.plan_id,
        &second.plan_hash,
        &GameFixture::approval(&second),
        &FileCancellation::default(),
    );
    assert!(matches!(
        second_result,
        Err(InstallError::FileTransaction(
            FileTransactionError::RootBusy(_)
        ))
    ));
    fixture.control.release();
    assert_eq!(
        first_job.join().unwrap().unwrap().disposition,
        ApplyDisposition::Committed
    );
}
