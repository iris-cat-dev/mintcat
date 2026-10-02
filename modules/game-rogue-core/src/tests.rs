use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Cursor, Write},
    path::Path,
};

use mintcat_game_api::{
    group_preconditions, BuiltGameOutput, DeployPrecondition, DiscoveryConfidence, GameInputFormat,
    GameSettingsSnapshot, LegacyFileDisposition, PlannedGameInput, RunningProcess,
};
use mintcat_integrator_core::uasset_utils::asset_registry::{AssetRegistry, Writable};
use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore, TrustStore};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::ArtifactRef;
use mintcat_unreal_pak::{normalize_entry_path, PakArchive, PakBuildTarget, PakBundleWriter};
use serde_json::json;
use tempfile::{tempdir, TempDir};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use super::*;
use crate::{
    builder::validate_patch_archive,
    descriptor::{MAIN_PAK, MOD_PAK},
};

struct Fixture {
    _sandbox: TempDir,
    root: std::path::PathBuf,
    store: ArtifactStore,
    builder: RogueCoreBuilder,
}

impl Fixture {
    fn new() -> Self {
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
        let root = sandbox.path().join("RogueCore");
        fs::create_dir_all(root.join("Content/Paks")).unwrap();
        fs::create_dir_all(root.join("Binaries/Win64")).unwrap();
        fs::write(root.join(MAIN_PAK), base_pak()).unwrap();
        let builder = RogueCoreBuilder::open(
            store.clone(),
            sandbox.path().join("staging"),
            RogueCoreBuildLimits::default(),
        )
        .unwrap();
        Self {
            _sandbox: sandbox,
            root,
            store,
            builder,
        }
    }

    fn blob(&self, bytes: &[u8], media_type: &str) -> (ArtifactRef, ArtifactGrant) {
        let artifact = self
            .store
            .publish_blob_reader(&mut Cursor::new(bytes), Some(media_type.to_owned()))
            .unwrap();
        let grant = self.store.grant_blob(&artifact).unwrap();
        (artifact, grant)
    }

    fn installation_and_base(
        &self,
    ) -> (
        mintcat_game_api::ValidatedGameInstallation,
        ArtifactRef,
        ArtifactGrant,
    ) {
        let installation =
            validate_installation(&self.root, RogueCoreVariant::SteamRelease).unwrap();
        let bytes = fs::read(self.root.join(MAIN_PAK)).unwrap();
        let (artifact, grant) = self.blob(&bytes, "application/vnd.unreal.pak");
        assert_eq!(artifact.digest, installation.fingerprint);
        (installation, artifact, grant)
    }
}

fn base_pak() -> Vec<u8> {
    let mut registry = Vec::new();
    AssetRegistry::empty().write(&mut registry).unwrap();
    pak(&[("RogueCore/AssetRegistry.bin", &registry)])
}

fn pak(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer =
        PakBundleWriter::new(Cursor::new(Vec::new()), &[], &PakBuildTarget::default()).unwrap();
    for (path, content) in files {
        writer.write_file(path, content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn input(id: &str, artifact: ArtifactRef) -> PlannedGameInput {
    PlannedGameInput {
        input_id: id.to_owned(),
        display_name: format!("RC Mod {id}"),
        source_id: "modcat".to_owned(),
        remote_game_key: "rogue-core".to_owned(),
        remote_mod_id: "rc-mod".to_owned(),
        format: GameInputFormat::Pak,
        artifact,
        audio_only_hint: false,
    }
}

fn plan(
    installation: mintcat_game_api::ValidatedGameInstallation,
    base: ArtifactRef,
    settings: RogueCoreSettings,
    patch: Option<String>,
    inputs: Vec<PlannedGameInput>,
) -> mintcat_game_api::GameBuildPlan {
    create_build_plan(RogueCoreBuildPlanRequest {
        installation,
        profile_revision: 21,
        module_lock_digest: "sha256:rc-module-lock".to_owned(),
        settings: settings.snapshot(8).unwrap(),
        base_game_artifact: base,
        patch_resource_lock_digest: patch,
        ordered_inputs: inputs,
    })
    .unwrap()
}

fn read_pak_file(
    store: &ArtifactStore,
    artifact: &ArtifactRef,
    normalized_path: &str,
) -> Option<Vec<u8>> {
    let grant = store.grant_blob(artifact).unwrap();
    let mut reader = grant.open_read().unwrap();
    let archive = PakArchive::open(&mut reader).unwrap();
    let stored = archive.metadata().files.iter().find(|stored| {
        normalize_entry_path(&archive.metadata().mount_point, stored)
            .is_ok_and(|path| path.eq_ignore_ascii_case(normalized_path))
    })?;
    let mut bytes = Vec::new();
    archive
        .read_file_to(stored, &mut reader, &mut bytes)
        .unwrap();
    Some(bytes)
}

fn zip(path: &Path, files: &[(&str, &[u8])]) {
    let file = File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for (name, bytes) in files {
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn discovers_both_editions_and_checks_only_rogue_core_processes() {
    let fixture = Fixture::new();
    let candidates = discover_roots([
        (
            fixture.root.clone(),
            RogueCoreVariant::SteamRelease,
            DiscoveryConfidence::UserSelected,
        ),
        (
            fixture.root.clone(),
            RogueCoreVariant::SteamPlaytest,
            DiscoveryConfidence::UserSelected,
        ),
    ])
    .unwrap();
    assert_eq!(candidates.len(), 2);
    let validated = validate_installation(&fixture.root, RogueCoreVariant::SteamPlaytest).unwrap();
    assert_eq!(validated.main_pak_relative_path, MAIN_PAK);
    assert_eq!(
        validate_main_pak_path(
            &fixture.root.join(MAIN_PAK),
            RogueCoreVariant::SteamPlaytest
        )
        .unwrap()
        .canonicalize()
        .unwrap(),
        fixture.root.canonicalize().unwrap()
    );
    assert!(validate_main_pak_path(
        Path::new("FSD/Content/Paks/FSD-WindowsNoEditor.pak"),
        RogueCoreVariant::SteamRelease
    )
    .is_err());

    ensure_not_running(
        RogueCoreVariant::SteamRelease,
        &[RunningProcess {
            pid: 10,
            executable_name: "FSD.exe".to_owned(),
        }],
    )
    .unwrap();
    assert!(ensure_not_running(
        RogueCoreVariant::SteamPlaytest,
        &[RunningProcess {
            pid: 11,
            executable_name: "roguecore-win64-shipping.exe".to_owned(),
        }],
    )
    .is_err());
}

#[test]
fn builds_supported_content_and_zero_input_build_drops_missing_mods() {
    let fixture = Fixture::new();
    let (installation, base, base_grant) = fixture.installation_and_base();
    let mod_bytes = pak(&[(
        "RogueCore/Content/Mods/Example/payload.bin",
        b"rogue core mod",
    )]);
    let (mod_artifact, mod_grant) = fixture.blob(&mod_bytes, "application/vnd.unreal.pak");
    let with_mod = plan(
        installation.clone(),
        base.clone(),
        RogueCoreSettings {
            compression: RogueCoreCompression::Zlib,
            ue4ss_enabled: false,
        },
        None,
        vec![input("mod-a", mod_artifact)],
    );
    let built = fixture
        .builder
        .build(RogueCoreBuildRequest {
            plan: &with_mod,
            base_game_artifact: &base_grant,
            inputs: vec![RogueCoreBuildInput {
                input_id: "mod-a".to_owned(),
                artifact: &mod_grant,
            }],
            patch_package: None,
        })
        .unwrap();
    assert_eq!(built.outputs[0].target_relative_path, MOD_PAK);
    assert_eq!(
        read_pak_file(
            &fixture.store,
            &built.outputs[0].artifact,
            "RogueCore/Content/Mods/Example/payload.bin"
        )
        .unwrap(),
        b"rogue core mod"
    );

    let clean = plan(
        installation,
        base,
        RogueCoreSettings::default(),
        None,
        Vec::new(),
    );
    let rebuilt = fixture
        .builder
        .build(RogueCoreBuildRequest {
            plan: &clean,
            base_game_artifact: &base_grant,
            inputs: Vec::new(),
            patch_package: None,
        })
        .unwrap();
    assert!(read_pak_file(
        &fixture.store,
        &rebuilt.outputs[0].artifact,
        "RogueCore/Content/Mods/Example/payload.bin"
    )
    .is_none());
    assert_ne!(
        rebuilt.outputs[0].artifact.digest,
        built.outputs[0].artifact.digest
    );
}

#[test]
fn unsupported_registry_fails_instead_of_falling_back() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join(MAIN_PAK),
        pak(&[("RogueCore/AssetRegistry.bin", b"unsupported")]),
    )
    .unwrap();
    let error = validate_installation(&fixture.root, RogueCoreVariant::SteamRelease).unwrap_err();
    assert!(matches!(error, RogueCoreError::UnsupportedVersion(_)));
}

#[test]
fn settings_and_deployment_are_scoped_to_rogue_core() {
    let fixture = Fixture::new();
    let (installation, base, _) = fixture.installation_and_base();
    let drg_settings = GameSettingsSnapshot::new(
        "game:drg",
        1,
        8,
        json!({"compression": "none", "ue4ssEnabled": true}),
    )
    .unwrap();
    assert!(create_build_plan(RogueCoreBuildPlanRequest {
        installation: installation.clone(),
        profile_revision: 21,
        module_lock_digest: "sha256:rc-module-lock".to_owned(),
        settings: drg_settings,
        base_game_artifact: base.clone(),
        patch_resource_lock_digest: None,
        ordered_inputs: Vec::new(),
    })
    .is_err());

    let pack_only = plan(
        installation.clone(),
        base.clone(),
        RogueCoreSettings::default(),
        None,
        Vec::new(),
    );
    assert!(create_deploy_plan(&pack_only, Vec::new(), Vec::new(), BTreeMap::new()).is_err());
    let installable = plan(
        installation,
        base,
        RogueCoreSettings::default(),
        Some(format!("sha256:{:064x}", 8)),
        Vec::new(),
    );
    let outputs = installable
        .outputs
        .iter()
        .map(|planned| {
            let (artifact, _) = fixture.blob(b"built rc pak", "application/vnd.unreal.pak");
            BuiltGameOutput {
                role: planned.role.clone(),
                target_relative_path: planned.target_relative_path.clone(),
                artifact,
                owner: planned.owner.clone(),
                executable: planned.executable,
            }
        })
        .collect::<Vec<_>>();
    let preconditions = group_preconditions(outputs.iter().map(|output| {
        (
            output.target_relative_path.clone(),
            DeployPrecondition::Absent,
        )
    }))
    .unwrap();
    let deployment = create_deploy_plan(&installable, outputs, Vec::new(), preconditions).unwrap();
    assert_eq!(deployment.game_id, ROGUE_CORE_GAME_ID);
    assert_eq!(deployment.files.len(), 1);
    assert_eq!(deployment.files[0].target_relative_path, MOD_PAK);
    assert_eq!(deployment.files[0].precondition, DeployPrecondition::Absent);
}

#[test]
fn patch_and_legacy_detection_reject_unsafe_assumptions() {
    let fixture = Fixture::new();
    let valid = fixture.root.join("valid-rc-patch.zip");
    zip(&valid, &[("Paks/Integration/runtime.bin", b"signed data")]);
    validate_patch_archive(&valid, RogueCoreBuildLimits::default()).unwrap();
    let invalid = fixture.root.join("invalid-rc-patch.zip");
    zip(&invalid, &[("../escape", b"bad")]);
    assert!(validate_patch_archive(&invalid, RogueCoreBuildLimits::default()).is_err());

    fs::write(fixture.root.join(MOD_PAK), b"old output").unwrap();
    fs::write(
        fixture
            .root
            .join("Content/Paks/RogueCore-Windows_audio_foreign_P.pak"),
        b"foreign audio",
    )
    .unwrap();
    fs::create_dir_all(fixture.root.join("Binaries/Win64/ue4ss/mods/third-party")).unwrap();
    let report = detect_legacy_install(&fixture.root, RogueCoreVariant::SteamRelease).unwrap();
    assert!(report.requires_approval);
    assert!(report.files.iter().any(|file| {
        file.relative_path == MOD_PAK && file.disposition == LegacyFileDisposition::TakeoverRequired
    }));
    assert!(report.files.iter().any(|file| {
        file.relative_path.contains("_audio_foreign_P.pak")
            && file.disposition == LegacyFileDisposition::ForeignPreserved
    }));
    assert!(!report
        .files
        .iter()
        .any(|file| file.relative_path.contains("third-party")));
}
