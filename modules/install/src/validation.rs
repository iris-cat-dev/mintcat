use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};

use mintcat_game_api::{build_plan_hash, canonical_digest, GameDeployPlan};
use mintcat_runtime_artifacts::ArtifactStore;
use mintcat_sdk::ArtifactRef;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::{
    AdapterBuildProduct, BuildConflict, BuildPurpose, BuildSummary, InstallError, ModuleClosure,
    ModuleClosureEntry, ProfileBuildSnapshot, WinnerSummary,
};

impl ModuleClosure {
    pub fn new(mut entries: Vec<ModuleClosureEntry>) -> Result<Self, InstallError> {
        entries.sort();
        if entries.is_empty() {
            return Err(InstallError::InvalidRequest(
                "module closure must contain at least one pinned package".to_owned(),
            ));
        }
        let mut ids = BTreeSet::new();
        for entry in &entries {
            if entry.module_id.trim().is_empty()
                || entry.version.trim().is_empty()
                || !valid_digest(&entry.package_digest)
                || !ids.insert(entry.module_id.to_ascii_lowercase())
            {
                return Err(InstallError::InvalidRequest(
                    "module closure entries require unique ids, versions, and SHA-256 digests"
                        .to_owned(),
                ));
            }
        }
        let digest = module_closure_digest(&entries)?;
        Ok(Self {
            schema_version: 1,
            entries,
            digest,
        })
    }

    pub fn verify(&self) -> Result<(), InstallError> {
        let canonical = Self::new(self.entries.clone())?;
        if self.schema_version != 1
            || self.entries != canonical.entries
            || self.digest != canonical.digest
        {
            return Err(InstallError::InvalidRequest(
                "module closure is not canonical or its digest differs".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn validate_build_product(
    purpose: BuildPurpose,
    profile: &ProfileBuildSnapshot,
    closure: &ModuleClosure,
    product: &AdapterBuildProduct,
) -> Result<BuildSummary, InstallError> {
    let plan = &product.build_plan;
    if plan.plan_hash != build_plan_hash(plan)? {
        return Err(InstallError::InvalidRequest(
            "game adapter returned a BuildPlan with an invalid hash".to_owned(),
        ));
    }
    if plan.game_id != profile.game_id
        || plan.profile_revision != profile.profile_revision
        || plan.module_lock_digest != closure.digest
        || plan.ordered_inputs != profile.planned_inputs()
    {
        return Err(InstallError::InvalidRequest(
            "game adapter BuildPlan differs from the immutable Profile/module closure".to_owned(),
        ));
    }
    plan.settings.verify()?;

    let expected = plan
        .outputs
        .iter()
        .map(|output| (output.target_relative_path.to_ascii_lowercase(), output))
        .collect::<BTreeMap<_, _>>();
    if expected.len() != plan.outputs.len() || product.outputs.len() != plan.outputs.len() {
        return Err(InstallError::InvalidRequest(
            "game adapter output set differs from the BuildPlan".to_owned(),
        ));
    }
    let mut observed = BTreeSet::new();
    for output in &product.outputs {
        let key = output.target_relative_path.to_ascii_lowercase();
        let Some(planned) = expected.get(&key) else {
            return Err(InstallError::InvalidRequest(format!(
                "game adapter returned unplanned output {}",
                output.target_relative_path
            )));
        };
        if !observed.insert(key)
            || output.role != planned.role
            || output.target_relative_path != planned.target_relative_path
            || output.owner != planned.owner
            || output.executable != planned.executable
            || !output.artifact.verified
            || !valid_digest(&output.artifact.digest)
        {
            return Err(InstallError::InvalidRequest(format!(
                "game adapter output {} differs from its BuildPlan declaration",
                output.target_relative_path
            )));
        }
    }

    match (purpose, product.deploy_plan.as_ref()) {
        (BuildPurpose::Pack, None) => {}
        (BuildPurpose::Pack, Some(_)) => {
            return Err(InstallError::InvalidRequest(
                "pack execution must not produce a DeployPlan".to_owned(),
            ));
        }
        (BuildPurpose::Install, Some(deploy)) => {
            validate_deploy_plan(deploy, plan, &product.outputs)?;
        }
        (BuildPurpose::Install, None) => {
            return Err(InstallError::InvalidRequest(
                "install execution did not produce a DeployPlan".to_owned(),
            ));
        }
    }

    build_summary(profile, &product.conflicts)
}

fn validate_deploy_plan(
    deploy: &GameDeployPlan,
    plan: &mintcat_game_api::GameBuildPlan,
    outputs: &[mintcat_game_api::BuiltGameOutput],
) -> Result<(), InstallError> {
    deploy.validate()?;
    if deploy.game_id != plan.game_id
        || deploy.variant_id != plan.variant_id
        || deploy.game_fingerprint != plan.game_fingerprint
        || deploy.profile_revision != plan.profile_revision
        || deploy.build_plan_hash != plan.plan_hash
        || deploy.module_lock_digest != plan.module_lock_digest
    {
        return Err(InstallError::InvalidRequest(
            "DeployPlan identity differs from its BuildPlan".to_owned(),
        ));
    }
    let deployed = deploy
        .files
        .iter()
        .map(|file| (file.target_relative_path.to_ascii_lowercase(), file))
        .collect::<BTreeMap<_, _>>();
    if deployed.len() != deploy.files.len() {
        return Err(InstallError::InvalidRequest(
            "DeployPlan contains duplicate targets".to_owned(),
        ));
    }
    for output in outputs {
        let file = deployed
            .get(&output.target_relative_path.to_ascii_lowercase())
            .ok_or_else(|| {
                InstallError::InvalidRequest(format!(
                    "DeployPlan omits built output {}",
                    output.target_relative_path
                ))
            })?;
        if file.artifact != output.artifact
            || file.owner != output.owner
            || file.executable != output.executable
        {
            return Err(InstallError::InvalidRequest(format!(
                "DeployPlan changed built output {}",
                output.target_relative_path
            )));
        }
    }
    Ok(())
}

pub(crate) fn verify_artifacts(
    artifacts: &ArtifactStore,
    refs: impl IntoIterator<Item = ArtifactRef>,
) -> Result<(), InstallError> {
    let mut unique = BTreeMap::new();
    for artifact in refs {
        if !artifact.verified || !valid_digest(&artifact.digest) {
            return Err(InstallError::InvalidRequest(format!(
                "artifact {} is not verified",
                artifact.digest
            )));
        }
        if let Some(size) = unique.insert(artifact.digest.clone(), artifact.size) {
            if size != artifact.size {
                return Err(InstallError::InvalidRequest(format!(
                    "artifact {} appears with conflicting sizes",
                    artifact.digest
                )));
            }
        }
    }
    let mut buffer = [0_u8; 64 * 1024];
    for (digest, size) in unique {
        let artifact = ArtifactRef {
            digest: digest.clone(),
            size,
            verified: true,
            media_type: None,
        };
        let grant = artifacts.grant_blob(&artifact)?;
        let mut reader = grant.open_read()?;
        let mut hasher = Sha256::new();
        let mut observed_size = 0_u64;
        loop {
            let read = reader.read(&mut buffer).map_err(|source| {
                InstallError::InvalidRequest(format!(
                    "reading artifact {digest} for verification failed: {source}"
                ))
            })?;
            if read == 0 {
                break;
            }
            observed_size = observed_size.checked_add(read as u64).ok_or_else(|| {
                InstallError::InvalidRequest(format!("artifact {digest} size overflow"))
            })?;
            hasher.update(&buffer[..read]);
        }
        let observed_digest = format!("sha256:{:x}", hasher.finalize());
        if observed_size != size || observed_digest != digest {
            return Err(InstallError::InvalidRequest(format!(
                "artifact {digest} bytes differ from the immutable descriptor"
            )));
        }
    }
    Ok(())
}

fn build_summary(
    profile: &ProfileBuildSnapshot,
    conflicts: &[BuildConflict],
) -> Result<BuildSummary, InstallError> {
    let positions = profile
        .ordered_inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            (
                input.input.input_id.as_str(),
                (index, input.input.display_name.as_str()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut paths = BTreeSet::new();
    let mut winners = Vec::with_capacity(conflicts.len());
    for conflict in conflicts {
        if conflict.relative_path.trim().is_empty()
            || conflict.shadowed_input_ids.is_empty()
            || !paths.insert(conflict.relative_path.to_ascii_lowercase())
        {
            return Err(InstallError::InvalidRequest(
                "game adapter conflict report is empty or duplicates a path".to_owned(),
            ));
        }
        let (winner_position, winner_name) = positions
            .get(conflict.winner_input_id.as_str())
            .copied()
            .ok_or_else(|| {
                InstallError::InvalidRequest(format!(
                    "conflict winner {} is not a planned input",
                    conflict.winner_input_id
                ))
            })?;
        let mut seen = BTreeSet::new();
        let mut shadowed_names = Vec::with_capacity(conflict.shadowed_input_ids.len());
        for shadowed in &conflict.shadowed_input_ids {
            let (position, name) = positions.get(shadowed.as_str()).copied().ok_or_else(|| {
                InstallError::InvalidRequest(format!(
                    "conflict loser {shadowed} is not a planned input"
                ))
            })?;
            if !seen.insert(shadowed) || position <= winner_position {
                return Err(InstallError::InvalidRequest(format!(
                    "conflict {} violates the first-input-wins rule",
                    conflict.relative_path
                )));
            }
            shadowed_names.push(name.to_owned());
        }
        winners.push(WinnerSummary {
            relative_path: conflict.relative_path.clone(),
            winner_input_id: conflict.winner_input_id.clone(),
            winner_display_name: winner_name.to_owned(),
            shadowed_input_ids: conflict.shadowed_input_ids.clone(),
            shadowed_display_names: shadowed_names,
            rule: "first-input-wins".to_owned(),
        });
    }
    winners.sort_by(|left, right| {
        left.relative_path
            .to_ascii_lowercase()
            .cmp(&right.relative_path.to_ascii_lowercase())
    });
    Ok(BuildSummary {
        ordered_input_ids: profile
            .ordered_inputs
            .iter()
            .map(|input| input.input.input_id.clone())
            .collect(),
        winners,
        warnings: profile.order_warnings.clone(),
    })
}

fn module_closure_digest(entries: &[ModuleClosureEntry]) -> Result<String, InstallError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HashableClosure<'a> {
        schema_version: u32,
        entries: &'a [ModuleClosureEntry],
    }
    Ok(canonical_digest(&HashableClosure {
        schema_version: 1,
        entries,
    })?)
}

pub(crate) fn valid_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
