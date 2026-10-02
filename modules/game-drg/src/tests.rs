use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

use mintcat_game_api::{
    group_preconditions, BuiltGameOutput, DeployPrecondition, DiscoveryConfidence, GameInputFormat,
    GameOutputRole, GameSettingsSnapshot, LegacyFileDisposition, PlannedGameInput, RunningProcess,
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
    descriptor::{STEAM_MAIN_PAK, STEAM_MOD_PAK},
};

struct Fixture {
    _sandbox: TempDir,
    root: PathBuf,
    store: ArtifactStore,
    builder: DrgBuilder,
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
        let root = sandbox.path().join("game");
        fs::create_dir_all(root.join("FSD/Content/Paks")).unwrap();
        fs::create_dir_all(root.join("FSD/Binaries/Win64")).unwrap();
        fs::write(root.join(STEAM_MAIN_PAK), base_pak()).unwrap();
        let builder = DrgBuilder::open(
            store.clone(),
            sandbox.path().join("staging"),
            DrgBuildLimits::default(),
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
        let installation = validate_installation(&self.root, DrgVariant::Steam).unwrap();
        let bytes = fs::read(self.root.join(STEAM_MAIN_PAK)).unwrap();
        let (artifact, grant) = self.blob(&bytes, "application/vnd.unreal.pak");
        assert_eq!(artifact.digest, installation.fingerprint);
        (installation, artifact, grant)
    }
}

fn base_pak() -> Vec<u8> {
    let mut registry = Vec::new();
    AssetRegistry::empty().write(&mut registry).unwrap();
    pak(&[("FSD/AssetRegistry.bin", &registry)])
}

fn pak(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer =
        PakBundleWriter::new(Cursor::new(Vec::new()), &[], &PakBuildTarget::default()).unwrap();
    for (path, content) in files {
        writer.write_file(path, content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn planned_input(id: &str, artifact: ArtifactRef) -> PlannedGameInput {
    PlannedGameInput {
        input_id: id.to_owned(),
        display_name: format!("Mod {id}"),
        source_id: "modio".to_owned(),
        remote_game_key: "drg".to_owned(),
        remote_mod_id: "12345".to_owned(),
        format: GameInputFormat::Pak,
        artifact,
        audio_only_hint: false,
    }
}

fn build_plan(
    installation: mintcat_game_api::ValidatedGameInstallation,
    base: ArtifactRef,
    settings: DrgSettings,
    patch: Option<String>,
    inputs: Vec<PlannedGameInput>,
) -> mintcat_game_api::GameBuildPlan {
    create_build_plan(DrgBuildPlanRequest {
        installation,
        profile_revision: 19,
        module_lock_digest: "sha256:module-lock".to_owned(),
        settings: settings.snapshot(7).unwrap(),
        base_game_artifact: base,
        patch_resource_lock_digest: patch,
        ordered_inputs: inputs,
    })
    .unwrap()
}

fn read_pak_file(store: &ArtifactStore, artifact: &ArtifactRef, normalized_path: &str) -> Vec<u8> {
    let grant = store.grant_blob(artifact).unwrap();
    let mut reader = grant.open_read().unwrap();
    let archive = PakArchive::open(&mut reader).unwrap();
    let stored = archive
        .metadata()
        .files
        .iter()
        .find(|stored| {
            normalize_entry_path(&archive.metadata().mount_point, stored)
                .is_ok_and(|path| path.eq_ignore_ascii_case(normalized_path))
        })
        .unwrap()
        .clone();
    let mut bytes = Vec::new();
    archive
        .read_file_to(&stored, &mut reader, &mut bytes)
        .unwrap();
    bytes
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
fn discovers_validates_and_guards_exact_drg_processes() {
    let fixture = Fixture::new();
    let candidates =
        discover_roots([(fixture.root.clone(), DiscoveryConfidence::UserSelected)]).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].variant_id, "steam");

    let validated = validate_installation(&fixture.root, DrgVariant::Steam).unwrap();
    assert_eq!(validated.main_pak_relative_path, STEAM_MAIN_PAK);
    assert_eq!(validated.target, "x86_64-pc-windows-msvc");
    let (root, variant) = validate_main_pak_path(&fixture.root.join(STEAM_MAIN_PAK)).unwrap();
    assert_eq!(
        root.canonicalize().unwrap(),
        fixture.root.canonicalize().unwrap()
    );
    assert_eq!(variant, DrgVariant::Steam);
    assert!(DrgVariant::from_main_pak(Path::new("Unknown.pak")).is_err());

    ensure_not_running(
        DrgVariant::Steam,
        &[RunningProcess {
            pid: 10,
            executable_name: "Unrelated.exe".to_owned(),
        }],
    )
    .unwrap();
    assert!(ensure_not_running(
        DrgVariant::Steam,
        &[RunningProcess {
            pid: 11,
            executable_name: "fsd-win64-shipping.exe".to_owned(),
        }],
    )
    .is_err());
}

#[test]
fn builds_a_legacy_compatible_mod_pak_from_immutable_inputs() {
    let fixture = Fixture::new();
    let (installation, base, base_grant) = fixture.installation_and_base();
    let mod_bytes = pak(&[(
        "FSD/Content/Mods/Example/payload.bin",
        b"representative mod",
    )]);
    let (mod_artifact, mod_grant) = fixture.blob(&mod_bytes, "application/vnd.unreal.pak");
    let plan = build_plan(
        installation,
        base,
        DrgSettings {
            compression: DrgCompression::Zlib,
            ue4ss_enabled: false,
        },
        None,
        vec![planned_input("mod-a", mod_artifact)],
    );

    let result = fixture
        .builder
        .build(DrgBuildRequest {
            plan: &plan,
            base_game_artifact: &base_grant,
            inputs: vec![DrgBuildInput {
                input_id: "mod-a".to_owned(),
                artifact: &mod_grant,
            }],
            patch_package: None,
        })
        .unwrap();

    assert_eq!(result.outputs.len(), 1);
    assert_eq!(result.outputs[0].target_relative_path, STEAM_MOD_PAK);
    assert_eq!(result.manifest.plan_hash, plan.plan_hash);
    assert_eq!(
        read_pak_file(
            &fixture.store,
            &result.outputs[0].artifact,
            "FSD/Content/Mods/Example/payload.bin"
        ),
        b"representative mod"
    );
    assert!(!read_pak_file(
        &fixture.store,
        &result.outputs[0].artifact,
        "FSD/AssetRegistry.bin"
    )
    .is_empty());
}

#[test]
fn settings_are_game_bound_and_deployment_requires_the_signed_patch_plan() {
    let fixture = Fixture::new();
    let (installation, base, _) = fixture.installation_and_base();
    let pack_only = build_plan(
        installation.clone(),
        base.clone(),
        DrgSettings::default(),
        None,
        Vec::new(),
    );
    assert!(create_deploy_plan(&pack_only, Vec::new(), Vec::new(), BTreeMap::new()).is_err());

    let rc_settings = GameSettingsSnapshot::new(
        "game:rogue-core",
        1,
        7,
        json!({"compression": "none", "ue4ssEnabled": true}),
    )
    .unwrap();
    let cross_game = create_build_plan(DrgBuildPlanRequest {
        installation: installation.clone(),
        profile_revision: 19,
        module_lock_digest: "sha256:module-lock".to_owned(),
        settings: rc_settings,
        base_game_artifact: base.clone(),
        patch_resource_lock_digest: None,
        ordered_inputs: Vec::new(),
    });
    assert!(cross_game.is_err());

    let installable = build_plan(
        installation,
        base,
        DrgSettings::default(),
        Some(format!("sha256:{:064x}", 9)),
        Vec::new(),
    );
    let outputs = installable
        .outputs
        .iter()
        .map(|planned| {
            let bytes = if planned.role == GameOutputRole::HookDll {
                b"MZ signed hook".as_slice()
            } else {
                b"built pak".as_slice()
            };
            let (artifact, _) = fixture.blob(bytes, "application/octet-stream");
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
    assert_eq!(deployment.files.len(), 2);
    assert_eq!(
        deployment
            .files
            .iter()
            .find(|file| file.executable)
            .unwrap()
            .approval_kinds,
        vec!["native-code-execution"]
    );
}

#[test]
fn patch_archives_and_legacy_takeover_are_strict_and_non_destructive() {
    let fixture = Fixture::new();
    let valid = fixture.root.join("valid-patch.zip");
    zip(
        &valid,
        &[
            ("DLLs/x3daudio1_7.dll", b"MZ hook"),
            (
                "Paks/_AssemblyStorm/ModIntegration/MI_SpawnMods.uasset",
                b"asset",
            ),
            (
                "Paks/_AssemblyStorm/ModIntegration/MI_SpawnMods.uexp",
                b"export",
            ),
        ],
    );
    validate_patch_archive(&valid, DrgBuildLimits::default()).unwrap();

    let traversal = fixture.root.join("traversal.zip");
    zip(
        &traversal,
        &[("../escape", b"bad"), ("DLLs/x3daudio1_7.dll", b"MZ hook")],
    );
    assert!(validate_patch_archive(&traversal, DrgBuildLimits::default()).is_err());

    fs::write(fixture.root.join(STEAM_MOD_PAK), b"old managed output").unwrap();
    fs::write(
        fixture
            .root
            .join("FSD/Content/Paks/FSD-WindowsNoEditor_audio_foreign_P.pak"),
        b"foreign audio",
    )
    .unwrap();
    fs::create_dir_all(
        fixture
            .root
            .join("FSD/Binaries/Win64/ue4ss/mods/third-party"),
    )
    .unwrap();
    fs::write(
        fixture
            .root
            .join("FSD/Binaries/Win64/ue4ss/mods/third-party/main.lua"),
        b"foreign loader mod",
    )
    .unwrap();
    let report = detect_legacy_install(&fixture.root, DrgVariant::Steam).unwrap();
    assert!(report.requires_approval);
    assert!(report.files.iter().any(|file| {
        file.relative_path == STEAM_MOD_PAK
            && file.disposition == LegacyFileDisposition::TakeoverRequired
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
