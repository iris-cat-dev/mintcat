use std::{fs, io::Cursor};

use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore, TrustStore};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::DIRECTORY_SNAPSHOT_MAGIC;
use mintcat_unreal_pak::{
    normalize_entry_path, PakArchive, PakBuildCodec, PakBuildTarget, PakBundleWriter, PakCodec,
};
use tempfile::{tempdir, TempDir};
use zip::{write::SimpleFileOptions, ZipWriter};

use super::*;

struct Fixture {
    _sandbox: TempDir,
    artifacts: ArtifactStore,
    engine: UnrealPakEngine,
}

impl Fixture {
    fn new() -> Self {
        let sandbox = tempdir().unwrap();
        let layout = RuntimePaths::new(sandbox.path().join("runtime"))
            .unwrap()
            .initialize(RuntimeEnvironment::Test)
            .unwrap();
        let artifacts = ArtifactStore::from_layout(
            &layout,
            RuntimeEnvironment::Test,
            "test-target",
            "1",
            TrustStore::default(),
        )
        .unwrap();
        let engine = UnrealPakEngine::open(
            artifacts.clone(),
            sandbox.path().join("engine-staging"),
            UnrealEngineLimits::default(),
        )
        .unwrap();
        Self {
            _sandbox: sandbox,
            artifacts,
            engine,
        }
    }

    fn grant(&self, bytes: &[u8]) -> ArtifactGrant {
        let artifact = self
            .artifacts
            .publish_blob_reader(&mut Cursor::new(bytes), None)
            .unwrap();
        self.artifacts.grant_blob(&artifact).unwrap()
    }
}

fn pak(files: &[(&str, &[u8])], codec: PakBuildCodec) -> Vec<u8> {
    let target = PakBuildTarget {
        codec,
        ..PakBuildTarget::default()
    };
    let mut writer = PakBundleWriter::new(Cursor::new(Vec::new()), &[], &target).unwrap();
    for (path, bytes) in files {
        writer.write_file(path, bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn zip_with_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in entries {
        writer
            .start_file(*path, SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn directory_snapshot(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(DIRECTORY_SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&(entries.len() as u64).to_be_bytes());
    for (path, content) in entries {
        bytes.extend_from_slice(&(path.len() as u32).to_be_bytes());
        bytes.extend_from_slice(path.as_bytes());
        bytes.extend_from_slice(&(content.len() as u64).to_be_bytes());
        bytes.extend_from_slice(content);
    }
    bytes
}

fn read_pak_file(grant: &ArtifactGrant, path: &str) -> Vec<u8> {
    let mut file = grant.open_read().unwrap();
    let archive = PakArchive::open(&mut file).unwrap();
    let stored = archive
        .metadata()
        .files
        .iter()
        .find(|stored| {
            normalize_entry_path(&archive.metadata().mount_point, stored).unwrap() == path
        })
        .unwrap()
        .clone();
    let mut bytes = Vec::new();
    archive
        .read_file_to(&stored, &mut file, &mut bytes)
        .unwrap();
    bytes
}

#[test]
fn build_is_deterministic_first_wins_and_verifies_compressed_output() {
    let fixture = Fixture::new();
    let first_payload = vec![b'a'; 64 * 1024];
    let second_payload = vec![b'b'; 64 * 1024];
    let first = fixture.grant(&pak(
        &[
            ("fsd/content/Shared.bin", &first_payload),
            ("FSD/Content/First.bin", b"first-only"),
        ],
        PakBuildCodec::None,
    ));
    let second = fixture.grant(&pak(
        &[
            ("FSD/Content/shared.bin", &second_payload),
            ("FSD/Content/Second.bin", b"second-only"),
        ],
        PakBuildCodec::None,
    ));

    let build = || {
        fixture
            .engine
            .build(UnrealBuildRequest {
                plan_hash: "sha256:fixture-plan".to_owned(),
                inputs: vec![
                    UnrealInput {
                        input_id: "first".to_owned(),
                        artifact: &first,
                        format: UnrealInputFormat::Pak,
                        content_prefix: None,
                    },
                    UnrealInput {
                        input_id: "second".to_owned(),
                        artifact: &second,
                        format: UnrealInputFormat::Pak,
                        content_prefix: None,
                    },
                ],
                reference_paths: vec!["FSD/Content/Shared.bin".to_owned()],
                target: PakBuildTarget {
                    codec: PakBuildCodec::Zlib,
                    ..PakBuildTarget::default()
                },
            })
            .unwrap()
    };
    let first_build = build();
    let second_build = build();

    assert_eq!(first_build.artifact.digest, second_build.artifact.digest);
    assert_eq!(first_build.manifest, second_build.manifest);
    assert_eq!(first_build.conflicts.len(), 1);
    assert_eq!(first_build.conflicts[0].winner_input_id, "first");
    assert_eq!(first_build.conflicts[0].shadowed_input_ids, ["second"]);
    assert_eq!(first_build.manifest.files.len(), 3);
    assert_eq!(
        first_build
            .manifest
            .files
            .iter()
            .find(|file| file
                .relative_path
                .eq_ignore_ascii_case("FSD/Content/Shared.bin"))
            .unwrap()
            .winner_input_id,
        "first"
    );

    let output = fixture.artifacts.grant_blob(&first_build.artifact).unwrap();
    let verification = fixture
        .engine
        .verify(&output, &first_build.manifest)
        .unwrap();
    assert_eq!(verification.file_count, 3);
    assert_eq!(verification.metadata.codecs, [PakCodec::Zlib]);
    assert_eq!(
        read_pak_file(&output, "FSD/Content/Shared.bin"),
        first_payload
    );
}

#[test]
fn zip_and_directory_artifacts_are_inspected_without_raw_paths() {
    let fixture = Fixture::new();
    let embedded = pak(
        &[("FSD/Content/ZipAsset.uasset", b"zip-asset")],
        PakBuildCodec::None,
    );
    let zip = fixture.grant(&zip_with_entries(&[("pak/mod.pak", &embedded)]));
    let zip_inspection = fixture
        .engine
        .inspect(UnrealInput {
            input_id: "zip".to_owned(),
            artifact: &zip,
            format: UnrealInputFormat::Zip,
            content_prefix: None,
        })
        .unwrap();
    assert_eq!(zip_inspection.files, ["FSD/Content/ZipAsset.uasset"]);
    assert_eq!(zip_inspection.pak_containers.len(), 1);

    let snapshot = fixture.grant(&directory_snapshot(&[
        ("Content/Loose/Asset.uasset", b"loose"),
        ("pak/embedded.pak", &embedded),
        ("js/main.js", b"ignored by PAK engine"),
    ]));
    let inspection = fixture
        .engine
        .inspect(UnrealInput {
            input_id: "directory".to_owned(),
            artifact: &snapshot,
            format: UnrealInputFormat::DirectorySnapshot,
            content_prefix: Some("RogueCore".to_owned()),
        })
        .unwrap();
    assert_eq!(
        inspection.files,
        [
            "FSD/Content/ZipAsset.uasset",
            "RogueCore/Content/Loose/Asset.uasset"
        ]
    );
    assert_eq!(inspection.ignored_entries, 1);
}

#[test]
fn malformed_archives_and_unsupported_targets_fail_explicitly() {
    let fixture = Fixture::new();
    let corrupt = fixture.grant(b"not a pak");
    assert!(matches!(
        fixture.engine.inspect(UnrealInput {
            input_id: "corrupt".to_owned(),
            artifact: &corrupt,
            format: UnrealInputFormat::Pak,
            content_prefix: None,
        }),
        Err(UnrealEngineError::Archive { .. })
    ));

    let unsafe_zip = fixture.grant(&zip_with_entries(&[("../escape.pak", b"bad")]));
    assert!(matches!(
        fixture.engine.inspect(UnrealInput {
            input_id: "unsafe-zip".to_owned(),
            artifact: &unsafe_zip,
            format: UnrealInputFormat::Zip,
            content_prefix: None,
        }),
        Err(UnrealEngineError::InvalidInput { .. })
    ));

    let valid = fixture.grant(&pak(
        &[("FSD/Content/File.bin", b"payload")],
        PakBuildCodec::None,
    ));
    let error = fixture
        .engine
        .build(UnrealBuildRequest {
            plan_hash: "sha256:bad-target".to_owned(),
            inputs: vec![UnrealInput {
                input_id: "valid".to_owned(),
                artifact: &valid,
                format: UnrealInputFormat::Pak,
                content_prefix: None,
            }],
            reference_paths: Vec::new(),
            target: PakBuildTarget {
                version: u8::MAX,
                ..PakBuildTarget::default()
            },
        })
        .unwrap_err();
    assert!(matches!(error, UnrealEngineError::Pak(_)));
}

#[test]
fn verification_detects_manifest_tampering() {
    let fixture = Fixture::new();
    let input = fixture.grant(&pak(
        &[("FSD/Content/File.bin", b"payload")],
        PakBuildCodec::None,
    ));
    let result = fixture
        .engine
        .build(UnrealBuildRequest {
            plan_hash: "sha256:verify".to_owned(),
            inputs: vec![UnrealInput {
                input_id: "input".to_owned(),
                artifact: &input,
                format: UnrealInputFormat::Pak,
                content_prefix: None,
            }],
            reference_paths: Vec::new(),
            target: PakBuildTarget::default(),
        })
        .unwrap();
    let output = fixture.artifacts.grant_blob(&result.artifact).unwrap();
    let mut tampered = result.manifest;
    tampered.files[0].digest = "sha256:forged".to_owned();
    assert!(matches!(
        fixture.engine.verify(&output, &tampered),
        Err(UnrealEngineError::Verification(_))
    ));
}

#[test]
fn fixture_pak_and_zip_remain_supported() {
    let fixture = Fixture::new();
    let fixture_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/baseline/mod-inputs");
    for (name, format) in [
        ("baseline.pak", UnrealInputFormat::Pak),
        ("baseline.zip", UnrealInputFormat::Zip),
    ] {
        let bytes = fs::read(fixture_root.join(name)).unwrap();
        let grant = fixture.grant(&bytes);
        let inspection = fixture
            .engine
            .inspect(UnrealInput {
                input_id: name.to_owned(),
                artifact: &grant,
                format,
                content_prefix: None,
            })
            .unwrap();
        assert!(inspection
            .files
            .iter()
            .any(|path| path == "FSD/Content/MintcatFixture/fixture.txt"));
    }
}
