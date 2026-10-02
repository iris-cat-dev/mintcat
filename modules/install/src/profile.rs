use std::collections::{BTreeMap, BTreeSet};

use mintcat_game_api::{canonical_digest, PlannedGameInput};
use mintcat_library_service::LibrarySnapshot;
use mintcat_sdk::ArtifactRef;
use serde::Serialize;

use crate::{
    BuildRequest, ContentInputMetadata, InstallError, ProfileBuildSnapshot, ProfileInputSnapshot,
};

pub(crate) fn capture_profile(
    library: &LibrarySnapshot,
    request: &BuildRequest,
) -> Result<ProfileBuildSnapshot, InstallError> {
    let profile = library
        .profiles
        .iter()
        .find(|profile| profile.profile_id == request.profile_id)
        .ok_or_else(|| {
            InstallError::InvalidRequest(format!("profile {} was not found", request.profile_id))
        })?;
    if profile.deleted_at_ms.is_some() {
        return Err(InstallError::InvalidRequest(format!(
            "profile {} is deleted",
            request.profile_id
        )));
    }
    if profile.owner_scope != request.owner_scope {
        return Err(InstallError::InvalidRequest(format!(
            "profile {} does not belong to owner scope {}",
            request.profile_id, request.owner_scope
        )));
    }
    if profile.revision != request.expected_profile_revision
        || profile.content_revision != request.expected_content_revision
    {
        return Err(InstallError::ProfileChanged {
            expected_revision: request.expected_profile_revision,
            actual_revision: profile.revision,
            expected_content_revision: request.expected_content_revision,
            actual_content_revision: profile.content_revision,
        });
    }
    if profile.game_id != request.settings.game_id {
        return Err(InstallError::InvalidRequest(format!(
            "profile game {} differs from settings game {}",
            profile.game_id, request.settings.game_id
        )));
    }

    let installation = library
        .installations
        .iter()
        .find(|installation| installation.installation_id == request.installation_id)
        .ok_or_else(|| {
            InstallError::InvalidRequest(format!(
                "installation {} was not found",
                request.installation_id
            ))
        })?;
    if installation.game_id != profile.game_id {
        return Err(InstallError::InvalidRequest(format!(
            "installation {} belongs to {}, not {}",
            installation.installation_id, installation.game_id, profile.game_id
        )));
    }
    let root_path = installation
        .root_path
        .as_ref()
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| {
            InstallError::InvalidRequest(format!(
                "installation {} has no resolved root",
                installation.installation_id
            ))
        })?
        .clone();

    let metadata = metadata_by_profile_mod(&request.input_metadata)?;
    let mut enabled = library
        .profile_mods
        .iter()
        .filter(|item| item.profile_id == profile.profile_id && item.enabled)
        .collect::<Vec<_>>();
    enabled.sort_by(|left, right| {
        left.sort_order
            .cmp(&right.sort_order)
            .then_with(|| left.profile_mod_id.cmp(&right.profile_mod_id))
    });
    let enabled_ids = enabled
        .iter()
        .map(|item| item.profile_mod_id.as_str())
        .collect::<BTreeSet<_>>();
    if metadata.keys().any(|id| !enabled_ids.contains(id.as_str())) {
        return Err(InstallError::InvalidRequest(
            "input metadata contains a disabled or unknown Profile Mod".to_owned(),
        ));
    }

    let bindings = library
        .profile_content_bindings
        .iter()
        .map(|binding| (binding.profile_mod_id.as_str(), binding))
        .collect::<BTreeMap<_, _>>();
    let content_refs = library
        .content_refs
        .iter()
        .map(|content| (content.digest.as_str(), content))
        .collect::<BTreeMap<_, _>>();
    let mods = library
        .mods
        .iter()
        .map(|item| (item.mod_id.as_str(), item))
        .collect::<BTreeMap<_, _>>();

    let mut ordered_inputs = Vec::with_capacity(enabled.len());
    let mut order_warnings = Vec::new();
    let mut previous_order: Option<(u64, &str)> = None;
    for item in enabled {
        if let Some((sort_order, previous_id)) = previous_order {
            if sort_order == item.sort_order {
                order_warnings.push(format!(
                    "duplicate sortOrder {} resolved by profileModId: {} before {}",
                    item.sort_order, previous_id, item.profile_mod_id
                ));
            }
        }
        previous_order = Some((item.sort_order, &item.profile_mod_id));

        let item_metadata = metadata.get(&item.profile_mod_id).ok_or_else(|| {
            InstallError::InvalidRequest(format!(
                "enabled Profile Mod {} has no immutable input metadata",
                item.profile_mod_id
            ))
        })?;
        let binding = bindings.get(item.profile_mod_id.as_str()).ok_or_else(|| {
            InstallError::InvalidRequest(format!(
                "enabled Profile Mod {} has no verified content binding",
                item.profile_mod_id
            ))
        })?;
        if item.used_version.as_deref() != Some(binding.version_id.as_str()) {
            return Err(InstallError::InvalidRequest(format!(
                "Profile Mod {} used version differs from its content binding",
                item.profile_mod_id
            )));
        }
        if item
            .selected_version_id
            .as_ref()
            .is_some_and(|selected| selected != &binding.version_id)
        {
            return Err(InstallError::InvalidRequest(format!(
                "Profile Mod {} selected version differs from its content binding",
                item.profile_mod_id
            )));
        }
        let library_mod = mods.get(item.mod_id.as_str()).ok_or_else(|| {
            InstallError::InvalidRequest(format!("Mod {} was not found", item.mod_id))
        })?;
        if library_mod.game_id != profile.game_id {
            return Err(InstallError::InvalidRequest(format!(
                "Mod {} belongs to {}, not {}",
                item.mod_id, library_mod.game_id, profile.game_id
            )));
        }
        let version = library
            .versions
            .iter()
            .find(|version| {
                version.mod_id == item.mod_id && version.version_id == binding.version_id
            })
            .ok_or_else(|| {
                InstallError::InvalidRequest(format!(
                    "version {} for Mod {} was not found",
                    binding.version_id, item.mod_id
                ))
            })?;
        if version
            .artifact_digest
            .as_ref()
            .is_some_and(|digest| digest != &binding.artifact_digest)
        {
            return Err(InstallError::InvalidRequest(format!(
                "version {} artifact differs from its Profile binding",
                binding.version_id
            )));
        }
        let content = content_refs
            .get(binding.artifact_digest.as_str())
            .filter(|content| content.verified)
            .ok_or_else(|| {
                InstallError::InvalidRequest(format!(
                    "Profile Mod {} content {} is not verified",
                    item.profile_mod_id, binding.artifact_digest
                ))
            })?;
        let artifact = ArtifactRef {
            digest: content.digest.clone(),
            size: content.size,
            verified: true,
            media_type: None,
        };
        ordered_inputs.push(ProfileInputSnapshot {
            profile_mod_id: item.profile_mod_id.clone(),
            mod_id: item.mod_id.clone(),
            version_id: binding.version_id.clone(),
            sort_order: item.sort_order,
            input: PlannedGameInput {
                input_id: item.profile_mod_id.clone(),
                display_name: library_mod.display_name.clone(),
                source_id: library_mod.source_id.clone(),
                remote_game_key: library_mod.remote_game_key.clone(),
                remote_mod_id: library_mod.remote_mod_id.clone(),
                format: item_metadata.format,
                artifact,
                audio_only_hint: item_metadata.audio_only_hint,
            },
        });
    }

    let mut snapshot = ProfileBuildSnapshot {
        schema_version: 1,
        profile_id: profile.profile_id.clone(),
        profile_incarnation_id: profile.incarnation_id.clone(),
        game_id: profile.game_id.clone(),
        installation_id: installation.installation_id.clone(),
        installation_incarnation_id: installation.incarnation_id.clone(),
        installation_state: installation.state.clone(),
        root_path,
        profile_revision: profile.revision,
        content_revision: profile.content_revision,
        ordered_inputs,
        order_warnings,
        digest: String::new(),
    };
    snapshot.digest = profile_digest(&snapshot)?;
    Ok(snapshot)
}

pub(crate) fn verify_profile_snapshot(snapshot: &ProfileBuildSnapshot) -> Result<(), InstallError> {
    if snapshot.schema_version != 1 || profile_digest(snapshot)? != snapshot.digest {
        return Err(InstallError::InvalidRequest(
            "Profile build snapshot digest is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn metadata_by_profile_mod(
    metadata: &[ContentInputMetadata],
) -> Result<BTreeMap<String, &ContentInputMetadata>, InstallError> {
    let mut result = BTreeMap::new();
    for item in metadata {
        if item.profile_mod_id.trim().is_empty()
            || result.insert(item.profile_mod_id.clone(), item).is_some()
        {
            return Err(InstallError::InvalidRequest(
                "input metadata Profile Mod ids must be non-empty and unique".to_owned(),
            ));
        }
    }
    Ok(result)
}

fn profile_digest(snapshot: &ProfileBuildSnapshot) -> Result<String, InstallError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HashableProfile<'a> {
        schema_version: u32,
        profile_id: &'a str,
        profile_incarnation_id: &'a str,
        game_id: &'a str,
        installation_id: &'a str,
        installation_incarnation_id: &'a str,
        installation_state: &'a str,
        root_path: &'a str,
        profile_revision: u64,
        content_revision: u64,
        ordered_inputs: &'a [ProfileInputSnapshot],
        order_warnings: &'a [String],
    }
    Ok(canonical_digest(&HashableProfile {
        schema_version: snapshot.schema_version,
        profile_id: &snapshot.profile_id,
        profile_incarnation_id: &snapshot.profile_incarnation_id,
        game_id: &snapshot.game_id,
        installation_id: &snapshot.installation_id,
        installation_incarnation_id: &snapshot.installation_incarnation_id,
        installation_state: &snapshot.installation_state,
        root_path: &snapshot.root_path,
        profile_revision: snapshot.profile_revision,
        content_revision: snapshot.content_revision,
        ordered_inputs: &snapshot.ordered_inputs,
        order_warnings: &snapshot.order_warnings,
    })?)
}
