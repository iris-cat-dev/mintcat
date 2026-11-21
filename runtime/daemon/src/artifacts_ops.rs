use std::{
    fs::{self, File},
    io::{self, Seek, SeekFrom},
    path::Path,
};

use mintcat_engine_unreal_pak::{UnrealEngineLimits, UnrealInput, UnrealInputFormat, UnrealPakEngine};
use mintcat_runtime_artifacts::ArtifactGrant;
use mintcat_sdk::{
    read_directory_snapshot_index, AppError, ArtifactRef, DetectedInstallInput,
    DirectorySnapshotLimits, InstallDetectInputsResult, ModConflictItem, ModsCheckConflictsResult,
    ModsExportArtifactParams, ModsExportArtifactResult,
};
use mintcat_unreal_pak::PakArchive;
use serde_json::json;
use uuid::Uuid;
use zip::ZipArchive;

use super::{internal_error, library_error, DaemonState};

fn failure(id: &str, code: &str, key: &str, details: serde_json::Value) -> Box<AppError> {
    Box::new(AppError {
        code: code.into(),
        message_key: key.into(),
        details,
        request_id: id.into(),
        job_id: None,
        retryable: false,
        recovery_actions: Vec::new(),
    })
}

pub(crate) fn detect_artifact_format(grant: &ArtifactGrant) -> Option<UnrealInputFormat> {
    let mut file = grant.open_read().ok()?;
    let limits = UnrealEngineLimits::default();
    let snapshot_limits = DirectorySnapshotLimits {
        max_entries: limits.max_entries,
        max_path_bytes: limits.max_path_bytes,
        max_entry_bytes: limits.max_entry_bytes,
        max_total_bytes: limits.max_expanded_bytes,
    };
    if read_directory_snapshot_index(&mut file, snapshot_limits).is_ok() {
        return Some(UnrealInputFormat::DirectorySnapshot);
    }
    if file.seek(SeekFrom::Start(0)).is_ok() && ZipArchive::new(&mut file).is_ok() {
        return Some(UnrealInputFormat::Zip);
    }
    if file.seek(SeekFrom::Start(0)).is_ok() && PakArchive::open(&mut file).is_ok() {
        return Some(UnrealInputFormat::Pak);
    }
    None
}

fn is_audio_only_files(files: &[String]) -> bool {
    if files.is_empty() {
        return false;
    }
    files.iter().all(|path| {
        let lower = path.to_ascii_lowercase();
        lower.ends_with(".wem")
            || lower.ends_with(".bnk")
            || lower.ends_with(".wav")
            || lower.ends_with(".ogg")
    })
}

fn open_engine(state: &DaemonState, request_id: &str) -> Result<UnrealPakEngine, Box<AppError>> {
    let staging = state.layout.runtime_dir.join("engine-inspect");
    UnrealPakEngine::open(
        state.artifacts.clone(),
        staging,
        UnrealEngineLimits::default(),
    )
    .map_err(|error| Box::new(internal_error(request_id, error.to_string())))
}

pub(crate) fn detect_install_inputs(
    state: &DaemonState,
    request_id: &str,
    profile_id: &str,
) -> Result<InstallDetectInputsResult, Box<AppError>> {
    let items = state
        .library
        .list_profile_mod_artifacts(&state.library_authorization, profile_id, None, true)
        .map_err(|error| Box::new(library_error(request_id, error)))?;
    let engine = open_engine(state, request_id)?;
    let mut detected = Vec::with_capacity(items.len());
    for item in items {
        let Some(digest) = item.artifact_digest else {
            detected.push(DetectedInstallInput {
                profile_mod_id: item.profile_mod_id,
                verified: false,
                artifact_digest: None,
                format: None,
                audio_only_hint: false,
            });
            continue;
        };
        let artifact_ref = ArtifactRef {
            digest: digest.clone(),
            size: item.file_size,
            verified: true,
            media_type: None,
        };
        let Ok(grant) = state.artifacts.grant_blob(&artifact_ref) else {
            detected.push(DetectedInstallInput {
                profile_mod_id: item.profile_mod_id,
                verified: false,
                artifact_digest: Some(digest),
                format: None,
                audio_only_hint: false,
            });
            continue;
        };
        let format = detect_artifact_format(&grant);
        let audio_only_hint = format
            .and_then(|fmt| {
                engine
                    .inspect(UnrealInput {
                        input_id: item.profile_mod_id.clone(),
                        artifact: &grant,
                        format: fmt,
                        content_prefix: None,
                    })
                    .ok()
                    .map(|inspection| is_audio_only_files(&inspection.files))
            })
            .unwrap_or(false);
        detected.push(DetectedInstallInput {
            profile_mod_id: item.profile_mod_id,
            verified: true,
            artifact_digest: Some(digest),
            format: format.map(|fmt| match fmt {
                UnrealInputFormat::Pak => "pak".to_owned(),
                UnrealInputFormat::Zip => "zip".to_owned(),
                UnrealInputFormat::DirectorySnapshot => "directory-snapshot".to_owned(),
            }),
            audio_only_hint,
        });
    }
    Ok(InstallDetectInputsResult {
        profile_id: profile_id.to_owned(),
        items: detected,
    })
}

pub(crate) fn check_profile_conflicts(
    state: &DaemonState,
    request_id: &str,
    profile_id: &str,
    profile_mod_ids: Option<&[String]>,
) -> Result<ModsCheckConflictsResult, Box<AppError>> {
    let enabled_only = profile_mod_ids.is_none_or(|ids| ids.is_empty());
    let items = state
        .library
        .list_profile_mod_artifacts(
            &state.library_authorization,
            profile_id,
            profile_mod_ids,
            enabled_only,
        )
        .map_err(|error| Box::new(library_error(request_id, error)))?;
    let engine = open_engine(state, request_id)?;
    let mut grants: Vec<(String, ArtifactGrant, UnrealInputFormat)> = Vec::new();
    let mut skipped = Vec::new();
    for item in items {
        let Some(digest) = item.artifact_digest else {
            skipped.push(item.profile_mod_id);
            continue;
        };
        let artifact_ref = ArtifactRef {
            digest,
            size: item.file_size,
            verified: true,
            media_type: None,
        };
        let Ok(grant) = state.artifacts.grant_blob(&artifact_ref) else {
            skipped.push(item.profile_mod_id);
            continue;
        };
        let Some(format) = detect_artifact_format(&grant) else {
            skipped.push(item.profile_mod_id);
            continue;
        };
        grants.push((item.profile_mod_id, grant, format));
    }
    let checked_count = grants.len() as u64;
    if grants.len() < 2 {
        return Ok(ModsCheckConflictsResult {
            profile_id: profile_id.to_owned(),
            checked_count,
            skipped_profile_mod_ids: skipped,
            conflicts: Vec::new(),
        });
    }
    let inputs = grants
        .iter()
        .map(|(id, grant, format)| UnrealInput {
            input_id: id.clone(),
            artifact: grant,
            format: *format,
            content_prefix: None,
        })
        .collect::<Vec<_>>();
    let conflicts = engine
        .conflicts(inputs)
        .map_err(|error| {
            failure(
                request_id,
                "INVALID_ARGUMENT",
                "mods.conflicts",
                json!({ "message": error.to_string() }),
            )
        })?
        .into_iter()
        .map(|conflict| ModConflictItem {
            relative_path: conflict.relative_path,
            winner_profile_mod_id: conflict.winner_input_id,
            shadowed_profile_mod_ids: conflict.shadowed_input_ids,
        })
        .collect();
    Ok(ModsCheckConflictsResult {
        profile_id: profile_id.to_owned(),
        checked_count,
        skipped_profile_mod_ids: skipped,
        conflicts,
    })
}

pub(crate) fn export_mod_artifact(
    state: &DaemonState,
    request_id: &str,
    params: &ModsExportArtifactParams,
) -> Result<ModsExportArtifactResult, Box<AppError>> {
    let dest = Path::new(params.destination_path.trim());
    if params.destination_path.trim().is_empty() || !dest.is_absolute() {
        return Err(failure(
            request_id,
            "INVALID_ARGUMENT",
            "mods.export.path",
            json!({ "message": "Export destination must be an absolute file path" }),
        ));
    }
    let parent = dest.parent().ok_or_else(|| {
        failure(
            request_id,
            "INVALID_ARGUMENT",
            "mods.export.path",
            json!({ "message": "Export destination has no parent directory" }),
        )
    })?;
    let parent_meta = fs::symlink_metadata(parent).map_err(|error| {
        failure(
            request_id,
            "INVALID_ARGUMENT",
            "mods.export.path",
            json!({ "message": format!("Export directory is unavailable: {error}") }),
        )
    })?;
    if !parent_meta.is_dir() || parent_meta.file_type().is_symlink() {
        return Err(failure(
            request_id,
            "INVALID_ARGUMENT",
            "mods.export.path",
            json!({ "message": "Export parent must be a real directory" }),
        ));
    }
    if let Ok(existing) = fs::symlink_metadata(dest) {
        if existing.is_dir() || existing.file_type().is_symlink() {
            return Err(failure(
                request_id,
                "INVALID_ARGUMENT",
                "mods.export.path",
                json!({ "message": "Export destination cannot be a directory or symlink" }),
            ));
        }
    }

    let info = state
        .library
        .get_profile_mod_artifact(
            &state.library_authorization,
            &params.profile_id,
            &params.profile_mod_id,
        )
        .map_err(|error| Box::new(library_error(request_id, error)))?;
    let digest = info.artifact_digest.ok_or_else(|| {
        failure(
            request_id,
            "ARTIFACT_NOT_VERIFIED",
            "mods.export.unverified",
            json!({ "profileModId": params.profile_mod_id }),
        )
    })?;
    let artifact_ref = ArtifactRef {
        digest: digest.clone(),
        size: info.file_size,
        verified: true,
        media_type: None,
    };
    let grant = state.artifacts.grant_blob(&artifact_ref).map_err(|error| {
        failure(
            request_id,
            "ARTIFACT_NOT_VERIFIED",
            "mods.export.artifact",
            json!({ "message": error.to_string() }),
        )
    })?;
    let detected_format = match detect_artifact_format(&grant) {
        Some(UnrealInputFormat::Pak) => "pak".to_owned(),
        Some(UnrealInputFormat::Zip) => "zip".to_owned(),
        Some(UnrealInputFormat::DirectorySnapshot) => "directory-snapshot".to_owned(),
        None => "unknown".to_owned(),
    };
    let temp_path = parent.join(format!(".mintcat-export-{}.tmp", Uuid::new_v4()));
    let copy_result = (|| -> io::Result<u64> {
        let mut reader = grant
            .open_read()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut writer = File::create(&temp_path)?;
        let written = io::copy(&mut reader, &mut writer)?;
        writer.sync_all()?;
        Ok(written)
    })();
    let written = match copy_result {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            return Err(Box::new(internal_error(request_id, error.to_string())));
        }
    };
    if let Err(error) = fs::rename(&temp_path, dest) {
        let _ = fs::remove_file(&temp_path);
        return Err(Box::new(internal_error(request_id, error.to_string())));
    }
    Ok(ModsExportArtifactResult {
        profile_mod_id: params.profile_mod_id.clone(),
        artifact_digest: digest,
        destination_path: dest.to_string_lossy().into_owned(),
        file_size: written,
        detected_format,
    })
}
