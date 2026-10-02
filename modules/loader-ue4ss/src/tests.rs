use std::{
    fs,
    io::{Cursor, Write},
    path::Path,
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use mintcat_runtime_artifacts::{module_signing_digest, ArtifactStore, TrustStore};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{
    ModuleActivation, ModuleFile, ModuleManifest, ModuleSignature, ModuleState, SignatureAlgorithm,
    DIRECTORY_SNAPSHOT_MAGIC,
};
use mintcat_unreal_pak::{PakBuildTarget, PakBundleWriter};
use sha2::{Digest as _, Sha256};
use tempfile::{tempdir, TempDir};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use super::*;

const PUBLISHER: &str = "mintcat.resources";
const KEY_ID: &str = "resources-2026";

struct Fixture {
    _sandbox: TempDir,
    source_root: std::path::PathBuf,
    store: ArtifactStore,
    loader: Ue4ssLoader,
    signing_key: SigningKey,
}

impl Fixture {
    fn new() -> Self {
        let sandbox = tempdir().unwrap();
        let layout = RuntimePaths::new(sandbox.path().join("runtime"))
            .unwrap()
            .initialize(RuntimeEnvironment::Test)
            .unwrap();
        let signing_key = SigningKey::from_bytes(&[19_u8; 32]);
        let mut trust = TrustStore::default();
        trust.trust_production_key(PUBLISHER, KEY_ID, signing_key.verifying_key());
        let store = ArtifactStore::from_layout(
            &layout,
            RuntimeEnvironment::Test,
            "test-target",
            "1",
            trust,
        )
        .unwrap();
        let loader = Ue4ssLoader::new(store.clone(), LoaderLimits::default()).unwrap();
        let source_root = sandbox.path().join("packages");
        fs::create_dir(&source_root).unwrap();
        Self {
            _sandbox: sandbox,
            source_root,
            store,
            loader,
            signing_key,
        }
    }

    fn blob(&self, content: &[u8], media_type: &str) -> mintcat_runtime_artifacts::ArtifactGrant {
        let artifact = self
            .store
            .publish_blob_reader(&mut Cursor::new(content), Some(media_type.to_owned()))
            .unwrap();
        self.store.grant_blob(&artifact).unwrap()
    }

    fn resource_package(
        &self,
        version: &str,
        files: &[(&str, &[u8], bool)],
    ) -> mintcat_runtime_artifacts::ModuleLease {
        let source = self.source_root.join(version.replace('.', "-"));
        fs::create_dir(&source).unwrap();
        let manifest_files = files
            .iter()
            .map(|(relative, content, executable)| {
                let path = source.join(relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, content).unwrap();
                ModuleFile {
                    path: (*relative).to_owned(),
                    size: content.len() as u64,
                    sha256: format!("sha256:{:x}", Sha256::digest(content)),
                    executable: *executable,
                }
            })
            .collect::<Vec<_>>();
        let manifest = ModuleManifest {
            schema_version: 1,
            id: "resource.ue4ss".to_owned(),
            version: version.to_owned(),
            publisher: PUBLISHER.to_owned(),
            target: "test-target".to_owned(),
            host_api: "1".to_owned(),
            components: Vec::new(),
            provides: Vec::new(),
            requires: Vec::new(),
            permissions: Vec::new(),
            contributes: serde_json::json!({"kind": "ue4ss-resource"}),
            state: ModuleState {
                schema_version: 1,
                readable_range: "1".to_owned(),
                writable_range: "1".to_owned(),
                rollback_before_write_only: Some(true),
            },
            activation: ModuleActivation {
                trigger: "on-demand".to_owned(),
                drain_policy: "finish-requests".to_owned(),
            },
            files: manifest_files,
        };
        let digest = module_signing_digest(&source, &manifest).unwrap();
        let signature = ModuleSignature {
            schema_version: 1,
            publisher: PUBLISHER.to_owned(),
            key_id: KEY_ID.to_owned(),
            algorithm: SignatureAlgorithm::Ed25519,
            signature: BASE64.encode(self.signing_key.sign(&digest).to_bytes()),
        };
        let installed = self
            .store
            .publish_module(&source, &manifest, &signature)
            .unwrap();
        self.store.acquire_module(&installed.digest).unwrap()
    }
}

fn resource_bindings() -> Vec<ResourceFileBinding> {
    vec![
        ResourceFileBinding {
            package_path: "payload/UE4SS.dll".to_owned(),
            target_relative_path: "UE4SS.dll".to_owned(),
            media_type: None,
        },
        ResourceFileBinding {
            package_path: "payload/dwmapi.dll".to_owned(),
            target_relative_path: "dwmapi.dll".to_owned(),
            media_type: None,
        },
        ResourceFileBinding {
            package_path: "payload/UE4SS-settings.ini".to_owned(),
            target_relative_path: "ue4ss/UE4SS-settings.ini".to_owned(),
            media_type: None,
        },
    ]
}

fn resource_files() -> Vec<(&'static str, &'static [u8], bool)> {
    vec![
        ("payload/UE4SS.dll", b"signed ue4ss dll", true),
        ("payload/dwmapi.dll", b"signed proxy dll", true),
        (
            "payload/UE4SS-settings.ini",
            b"[General]\nKeep = yes\n[Hooks]\nHookProcessInternal = 0\n",
            false,
        ),
    ]
}

fn zip_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for (path, content) in files {
        writer.start_file(*path, options).unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn snapshot_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut result = DIRECTORY_SNAPSHOT_MAGIC.to_vec();
    result.extend_from_slice(&(files.len() as u64).to_be_bytes());
    for (path, content) in files {
        result.extend_from_slice(&(path.len() as u32).to_be_bytes());
        result.extend_from_slice(path.as_bytes());
        result.extend_from_slice(&(content.len() as u64).to_be_bytes());
        result.extend_from_slice(content);
    }
    result
}

fn pe_dll() -> Vec<u8> {
    let mut bytes = vec![0_u8; 128];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&64_u32.to_le_bytes());
    bytes[64..68].copy_from_slice(b"PE\0\0");
    bytes[86..88].copy_from_slice(&0x2000_u16.to_le_bytes());
    bytes
}

fn audio_pak(path: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = PakBundleWriter::new(cursor, &[], &PakBuildTarget::default()).unwrap();
    writer.write_file(path, b"audio asset").unwrap();
    writer.finish().unwrap().into_inner()
}

fn read_artifact(store: &ArtifactStore, artifact: &mintcat_sdk::ArtifactRef) -> Vec<u8> {
    let grant = store.grant_blob(artifact).unwrap();
    fs::read(&grant.descriptor.path).unwrap()
}

#[test]
fn signed_resources_lock_versions_and_apply_game_specific_settings() {
    let fixture = Fixture::new();
    let drg_package = fixture.resource_package("3.0.1", &resource_files());
    let rc_package = fixture.resource_package("3.1.0", &resource_files());

    let drg = fixture
        .loader
        .plan_resource_package(ResourceDeploymentRequest {
            game_id: "deep-rock-galactic".to_owned(),
            owner: "loader:ue4ss".to_owned(),
            package: &drg_package,
            files: resource_bindings(),
            settings: Ue4ssSettings::deep_rock_galactic(),
        })
        .unwrap();
    let rc = fixture
        .loader
        .plan_resource_package(ResourceDeploymentRequest {
            game_id: "rogue-core".to_owned(),
            owner: "loader:ue4ss".to_owned(),
            package: &rc_package,
            files: resource_bindings(),
            settings: Ue4ssSettings::rogue_core(),
        })
        .unwrap();

    assert_eq!(drg.resource_locks[0].version, "3.0.1");
    assert_eq!(rc.resource_locks[0].version, "3.1.0");
    assert_ne!(
        drg.resource_locks[0].package_digest,
        rc.resource_locks[0].package_digest
    );
    assert_eq!(drg.approval_requirements.len(), 2);
    assert!(drg.approval_requirements.iter().all(|approval| {
        approval.risk == DeploymentRisk::ExecutableResource
            && approval.signed_resource.as_ref() == drg.resource_locks.first()
    }));

    let drg_settings = drg
        .files
        .iter()
        .find(|file| file.target_relative_path.ends_with("UE4SS-settings.ini"))
        .unwrap();
    let rc_settings = rc
        .files
        .iter()
        .find(|file| file.target_relative_path.ends_with("UE4SS-settings.ini"))
        .unwrap();
    let drg_content =
        String::from_utf8(read_artifact(&fixture.store, &drg_settings.artifact)).unwrap();
    let rc_content =
        String::from_utf8(read_artifact(&fixture.store, &rc_settings.artifact)).unwrap();
    assert!(!drg_content.contains("EngineVersionOverride"));
    assert!(rc_content.contains("MajorVersion = 5"));
    assert!(rc_content.contains("MinorVersion = 6"));
    assert!(rc_content.contains("HookProcessInternal = 1"));
    assert!(rc_content.contains("HookProcessLocalScriptFunction = 1"));
    assert!(rc_content.contains("Keep = yes"));
}

#[test]
fn js_native_and_audio_plans_emit_only_declared_targets() {
    let fixture = Fixture::new();
    let js_archive = zip_bytes(&[
        ("wrapped/js/main.js", b"main"),
        ("wrapped/js/lib/helper.js", b"helper"),
        ("wrapped/content/mod.pak", b"not deployed here"),
    ]);
    let js_grant = fixture.blob(&js_archive, "application/zip");
    let js = fixture
        .loader
        .plan_js_mod(JsDeploymentRequest {
            game_id: "rogue-core".to_owned(),
            owner: "mod:js-1".to_owned(),
            mod_id: "js-1".to_owned(),
            input_id: "js-input".to_owned(),
            artifact: &js_grant,
            format: JsInputFormat::Zip,
        })
        .unwrap();
    assert_eq!(
        js.files
            .iter()
            .map(|file| file.target_relative_path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "ue4ss/mods/js-1/js/lib/helper.js",
            "ue4ss/mods/js-1/js/main.js"
        ]
    );
    assert_eq!(js.ignored_inputs, vec!["wrapped/content/mod.pak"]);
    assert!(js
        .approval_requirements
        .iter()
        .all(|approval| approval.risk == DeploymentRisk::ScriptExecution));

    let native_grant = fixture.blob(&pe_dll(), PE_MEDIA_TYPE);
    let native = fixture
        .loader
        .plan_native_mod(NativeDeploymentRequest {
            game_id: "rogue-core".to_owned(),
            owner: "mod:native-1".to_owned(),
            mod_id: "native-1".to_owned(),
            input_id: "native-input".to_owned(),
            artifact: &native_grant,
        })
        .unwrap();
    assert_eq!(
        native.files[0].target_relative_path,
        "ue4ss/mods/native-1/main.dll"
    );
    assert_eq!(
        native.approval_requirements[0].risk,
        DeploymentRisk::NativeCodeExecution
    );

    let pak = audio_pak("Content/Audio/voice.uasset");
    let audio_zip = zip_bytes(&[("audio.pak", &pak)]);
    let audio_grant = fixture.blob(&audio_zip, "application/zip");
    let audio = fixture
        .loader
        .plan_audio_pak(AudioDeploymentRequest {
            game_id: "rogue-core".to_owned(),
            owner: "mod:audio-1".to_owned(),
            source_id: "modio".to_owned(),
            mod_id: "42".to_owned(),
            input_id: "audio-input".to_owned(),
            artifact: &audio_grant,
            format: AudioInputFormat::Zip,
            paks_directory: "RogueCore/Content/Paks".to_owned(),
        })
        .unwrap();
    assert_eq!(
        audio.files[0].target_relative_path,
        "RogueCore/Content/Paks/modio-42_P.pak"
    );
    assert!(audio.approval_requirements.is_empty());

    let combined = js.merge(native).unwrap().merge(audio).unwrap();
    assert_eq!(combined.files.len(), 4);
    assert!(combined
        .owned_targets()
        .all(|(target, _)| !target.ends_with('/') && target != "ue4ss/mods"));
}

#[test]
fn directory_snapshot_deploys_only_its_js_tree() {
    let fixture = Fixture::new();
    let snapshot = snapshot_bytes(&[
        ("js/main.js", b"main"),
        ("js/config.json", b"{}"),
        ("Content/mod.pak", b"ignored"),
    ]);
    let grant = fixture.blob(&snapshot, mintcat_sdk::DIRECTORY_SNAPSHOT_MEDIA_TYPE);
    let manifest = fixture
        .loader
        .plan_js_mod(JsDeploymentRequest {
            game_id: "deep-rock-galactic".to_owned(),
            owner: "mod:snapshot".to_owned(),
            mod_id: "snapshot".to_owned(),
            input_id: "snapshot-input".to_owned(),
            artifact: &grant,
            format: JsInputFormat::DirectorySnapshot,
        })
        .unwrap();
    assert_eq!(manifest.files.len(), 2);
    assert_eq!(manifest.ignored_inputs, vec!["Content/mod.pak"]);
    assert_eq!(
        read_artifact(&fixture.store, &manifest.files[1].artifact),
        b"main"
    );
}

#[test]
fn rejects_uncontrolled_archives_and_never_deletes_failed_source_artifacts() {
    let fixture = Fixture::new();
    let traversal = zip_bytes(&[("../outside.js", b"bad"), ("js/main.js", b"main")]);
    let grant = fixture.blob(&traversal, "application/zip");
    let source = grant.descriptor.path.clone();
    assert!(matches!(
        fixture.loader.plan_js_mod(JsDeploymentRequest {
            game_id: "rogue-core".to_owned(),
            owner: "mod:bad".to_owned(),
            mod_id: "bad".to_owned(),
            input_id: "bad-zip".to_owned(),
            artifact: &grant,
            format: JsInputFormat::Zip,
        }),
        Err(LoaderError::InvalidInput { .. })
    ));
    assert!(Path::new(&source).is_file());

    let corrupt = fixture.blob(b"not a ZIP", "application/zip");
    let corrupt_source = corrupt.descriptor.path.clone();
    assert!(matches!(
        fixture.loader.plan_js_mod(JsDeploymentRequest {
            game_id: "rogue-core".to_owned(),
            owner: "mod:bad".to_owned(),
            mod_id: "bad".to_owned(),
            input_id: "corrupt-zip".to_owned(),
            artifact: &corrupt,
            format: JsInputFormat::Zip,
        }),
        Err(LoaderError::Archive { .. })
    ));
    assert!(Path::new(&corrupt_source).is_file());
}

#[test]
fn rejects_unsigned_resource_expansion_invalid_dlls_and_non_audio_game_state() {
    let fixture = Fixture::new();
    let package = fixture.resource_package(
        "4.0.0",
        &[
            ("payload/UE4SS.dll", b"dll", true),
            ("payload/undeclared.bin", b"hidden", false),
        ],
    );
    let resource_error = fixture
        .loader
        .plan_resource_package(ResourceDeploymentRequest {
            game_id: "rogue-core".to_owned(),
            owner: "loader:ue4ss".to_owned(),
            package: &package,
            files: vec![ResourceFileBinding {
                package_path: "payload/UE4SS.dll".to_owned(),
                target_relative_path: "UE4SS.dll".to_owned(),
                media_type: None,
            }],
            settings: Ue4ssSettings::deep_rock_galactic(),
        })
        .unwrap_err();
    assert!(matches!(
        resource_error,
        LoaderError::ResourceVerification(_)
    ));

    let invalid_dll = fixture.blob(b"not PE", PE_MEDIA_TYPE);
    assert!(matches!(
        fixture.loader.plan_native_mod(NativeDeploymentRequest {
            game_id: "rogue-core".to_owned(),
            owner: "mod:native".to_owned(),
            mod_id: "native".to_owned(),
            input_id: "native".to_owned(),
            artifact: &invalid_dll,
        }),
        Err(LoaderError::InvalidInput { .. })
    ));

    let prohibited = audio_pak("Content/Game/AssetRegistry.bin");
    let prohibited_grant = fixture.blob(&prohibited, PAK_MEDIA_TYPE);
    assert!(matches!(
        fixture.loader.plan_audio_pak(AudioDeploymentRequest {
            game_id: "rogue-core".to_owned(),
            owner: "mod:audio".to_owned(),
            source_id: "local".to_owned(),
            mod_id: "bad-audio".to_owned(),
            input_id: "bad-audio".to_owned(),
            artifact: &prohibited_grant,
            format: AudioInputFormat::Pak,
            paks_directory: "RogueCore/Content/Paks".to_owned(),
        }),
        Err(LoaderError::InvalidInput { .. })
    ));
}

#[test]
fn merge_rejects_case_colliding_targets_and_cross_game_manifests() {
    let artifact = mintcat_sdk::ArtifactRef {
        digest: format!("sha256:{:064x}", 1),
        size: 1,
        verified: true,
        media_type: None,
    };
    let manifest = |game: &str, target: &str| DeploymentManifest {
        schema_version: 1,
        game_id: game.to_owned(),
        resource_locks: Vec::new(),
        files: vec![DeploymentFile {
            target_relative_path: target.to_owned(),
            artifact: artifact.clone(),
            owner: "mod:test".to_owned(),
            content_kind: DeploymentContentKind::Script,
            executable: false,
        }],
        approval_requirements: vec![ApprovalRequirement {
            risk: DeploymentRisk::ScriptExecution,
            target_relative_path: target.to_owned(),
            artifact_digest: artifact.digest.clone(),
            signed_resource: None,
        }],
        ignored_inputs: Vec::new(),
    };
    assert!(matches!(
        manifest("game", "ue4ss/mods/a/js/main.js")
            .merge(manifest("game", "UE4SS/mods/a/js/main.js")),
        Err(LoaderError::TargetCollision(_))
    ));
    assert!(matches!(
        manifest("game-a", "a").merge(manifest("game-b", "b")),
        Err(LoaderError::InvalidRequest(_))
    ));
}
