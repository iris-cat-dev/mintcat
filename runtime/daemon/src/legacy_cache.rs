use std::{fs::File, io::Read};

use anyhow::{bail, ensure, Context, Result};
use mintcat_engine_unreal_pak::{UnrealEngineLimits, UnrealInput, UnrealInputFormat, UnrealPakEngine};
use mintcat_feature_backup::MaintenanceGate;
use mintcat_library_service::{
    LegacyCacheCandidate, LibraryAuthorization, LibraryStore, CONTENT_WRITE_PERMISSION,
    READ_PERMISSION,
};
use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore, TrustStore};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimeLayout};
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport<'a> {
    profile_id: &'a str,
    content_revision: u64,
    applied: bool,
    candidate_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidates: Option<Vec<LegacyCacheCandidate>>,
}

pub fn restore<'a>(
    layout: &RuntimeLayout,
    environment: RuntimeEnvironment,
    profile_id: &'a str,
    expected_content_revision: u64,
    apply: bool,
) -> Result<RestoreReport<'a>> {
    ensure!(layout.initialized, "restore requires an existing vNext data directory");
    let _maintenance = MaintenanceGate::exclusive(layout, false)
        .context("could not acquire exclusive runtime maintenance admission")?;
    // This read-only open still holds the Library writer lock until the command ends.
    let library = LibraryStore::open_existing_read_only(layout)
        .context("could not open the existing Library owner exclusively; stop mintcatd first")?;
    let authorization = LibraryAuthorization::new("local-user", [READ_PERMISSION, CONTENT_WRITE_PERMISSION]);
    let candidates = library
        .legacy_cache_candidates(&authorization, profile_id, expected_content_revision)
        .context("could not enumerate the complete set of imported local cache files")?;
    let candidate_count = candidates.len();

    if !apply || candidates.is_empty() {
        return Ok(RestoreReport {
            profile_id,
            content_revision: expected_content_revision,
            applied: apply,
            candidate_count,
            candidates: (!apply).then_some(candidates),
        });
    }

    // Match the DRG deployment per-input/container bounds, not the broader engine defaults.
    let limits = restore_limits();
    let artifacts = ArtifactStore::from_layout(
        layout,
        environment,
        target_lexicon::HOST.to_string(),
        "1",
        TrustStore::default(),
    )
    .context("could not open the local artifact store")?;
    let engine = UnrealPakEngine::open(
        artifacts.clone(),
        layout.runtime_dir.join("legacy-cache-inspect"),
        limits,
    )
    .context("could not prepare bounded legacy archive inspection")?;
    let mut sources = Vec::with_capacity(candidate_count);
    let mut inputs = Vec::with_capacity(candidate_count);
    for candidate in candidates {
        let (source, grant) = snapshot(&candidate, &artifacts, &engine, limits.max_input_bytes)
            .with_context(|| {
                format!(
                    "could not restore profile mod {} from {}",
                    candidate.profile_mod_id,
                    candidate.path.display()
                )
            })?;
        // Keep the checked handle open during the owner recheck and binding transaction.
        sources.push(source);
        inputs.push((candidate, grant));
    }
    // The maintenance gate was acquired with recovery disabled: no marker is cleared.
    library.resume_after_recovery().context("could not enable Library writes")?;
    let content_revision = library
        .restore_legacy_cache(&authorization, profile_id, expected_content_revision, &inputs)
        .context("could not atomically bind the complete local cache batch")?;
    drop(sources);
    Ok(RestoreReport {
        profile_id,
        content_revision,
        applied: true,
        candidate_count,
        candidates: None,
    })
}

pub(crate) fn restore_available(
    state: &super::DaemonState,
    request_id: &str,
    profile_id: &str,
    revision: u64,
    key: &str,
) -> std::result::Result<mintcat_sdk::InstallRestoreLegacyCacheResult, Box<mintcat_sdk::AppError>> {
    let authorization = &state.library_authorization;
    let library = &state.library;
    let library_error = |error| Box::new(super::library_error(request_id, error));
    let summarize = |result: mintcat_library_service::ApplyOperationResult| mintcat_sdk::InstallRestoreLegacyCacheResult {
        content_revision: result.content_revision,
        restored_count: result.artifact_digests.len() as u64,
    };
    if let Some(previous) = library.previous_legacy_cache_restore(authorization, profile_id, revision, key).map_err(library_error)? {
        return Ok(summarize(previous));
    }
    let candidates = library.available_legacy_cache_candidates(authorization, profile_id, revision).map_err(library_error)?;
    let mut inputs = Vec::with_capacity(candidates.len());
    if !candidates.is_empty() {
        let limits = restore_limits();
        let engine = UnrealPakEngine::open(
            state.artifacts.clone(), state.layout.runtime_dir.join("legacy-cache-inspect"), limits,
        ).map_err(|error| Box::new(super::internal_error(request_id, error.to_string())))?;
        for candidate in candidates {
            // An unavailable or invalid old file must not prevent reuse of a different file.
            if let Ok((_, grant)) = snapshot(&candidate, &state.artifacts, &engine, limits.max_input_bytes) {
                inputs.push((candidate, grant));
            }
        }
    }
    library.restore_available_legacy_cache(authorization, profile_id, revision, key, &inputs)
        .map(summarize).map_err(library_error)
}

fn restore_limits() -> UnrealEngineLimits {
    UnrealEngineLimits {
        max_input_bytes: 4 * 1024 * 1024 * 1024,
        max_entry_bytes: 2 * 1024 * 1024 * 1024,
        max_expanded_bytes: 16 * 1024 * 1024 * 1024,
        ..UnrealEngineLimits::default()
    }
}

fn snapshot(
    candidate: &LegacyCacheCandidate,
    artifacts: &ArtifactStore,
    engine: &UnrealPakEngine,
    max_input_bytes: u64,
) -> Result<(File, ArtifactGrant)> {
    ensure!(
        (1..=max_input_bytes).contains(&candidate.file_size),
        "recorded cache file is outside the 1 to {max_input_bytes} byte input limit"
    );
    let (format, media_type) = match candidate.path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("zip") => (UnrealInputFormat::Zip, "application/zip"),
        Some(ext) if ext.eq_ignore_ascii_case("pak") => {
            (UnrealInputFormat::Pak, "application/vnd.unreal.pak")
        }
        _ => bail!("recorded cache path must name a ZIP or PAK file"),
    };
    let mut source = candidate
        .open_read()
        .context("could not open the exact recorded file")?;
    // Bound the copy itself; an extra byte detects concurrent growth of the source.
    let mut bounded = (&mut source).take(candidate.file_size + 1);
    let artifact = artifacts
        .publish_blob_reader(&mut bounded, Some(media_type.into()))
        .context("could not snapshot the local bytes")?;
    ensure!(
        artifact.size == candidate.file_size,
        "recorded cache file changed size during snapshot"
    );
    // The CAS verifies byte identity only. It does not authenticate a remote provider.
    let grant = artifacts
        .grant_blob(&artifact)
        .context("could not grant the local snapshot")?;
    let inspection = engine.inspect(UnrealInput {
        input_id: candidate.profile_mod_id.clone(),
        artifact: &grant,
        format,
        content_prefix: None,
    })?;
    ensure!(
        !inspection.pak_containers.is_empty() && !inspection.files.is_empty(),
        "cache file does not contain a non-empty Unreal PAK"
    );
    Ok((source, grant))
}
