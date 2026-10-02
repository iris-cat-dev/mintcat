use std::{
    fs::{self, File},
    io::Write as _,
    time::{SystemTime, UNIX_EPOCH},
};

use mintcat_runtime_artifacts::{ArtifactStore, TrustStore};
use mintcat_runtime_broker::{
    open_interaction_broker, BrokerError, InteractionContext, InteractionHost,
};
use mintcat_runtime_jobs::{ConfirmationPolicy, EventUpdate, JobError, JobStore, RegisterJob};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{ActionRequired, JobState};
use serde_json::json;
use tempfile::TempDir;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use crate::{LocalContentKind, LocalImportLimits, LocalInputKind, LocalSource, LocalSourceError};

struct Fixture {
    _sandbox: TempDir,
    local: LocalSource,
    host: InteractionHost,
    jobs: JobStore,
    artifacts: ArtifactStore,
}

impl Fixture {
    fn register(&self, suffix: &str) -> (String, InteractionContext) {
        let registration = self
            .jobs
            .register_job(RegisterJob {
                command: "source.local.import".to_owned(),
                owner_scope: "owner:test".to_owned(),
                idempotency_key: format!("local-{suffix}"),
                request_hash: format!("sha256:{suffix}"),
                target: json!({"input": suffix}),
                confirmation_policy: ConfirmationPolicy::Required,
            })
            .unwrap();
        self.jobs
            .append_event(
                &registration.job.job_id,
                EventUpdate::state("prepare", JobState::Preparing),
            )
            .unwrap();
        (
            registration.job.job_id,
            InteractionContext {
                operation_id: format!("operation:{suffix}"),
                target_revision: 1,
                plan_hash: format!("sha256:plan-{suffix}"),
                module_lock_digest: "sha256:lock".to_owned(),
                expires_at_ms: now_ms() + 60_000,
            },
        )
    }

    fn authorize_file(&self, suffix: &str, path: &std::path::Path) -> ActionRequired {
        let (job_id, context) = self.register(suffix);
        let action = self
            .local
            .request_file_input(&job_id, &context, "Import local mod")
            .unwrap();
        self.host
            .complete_file_selection(
                "owner:test",
                &action.action_id,
                action.revision,
                path,
                &format!("complete-{suffix}"),
            )
            .unwrap();
        action
    }

    fn authorize_directory(&self, suffix: &str, path: &std::path::Path) -> ActionRequired {
        let (job_id, context) = self.register(suffix);
        let action = self
            .local
            .request_directory_input(&job_id, &context, "Import unpacked mod")
            .unwrap();
        self.host
            .complete_directory_selection(
                "owner:test",
                &action.action_id,
                action.revision,
                path,
                &format!("complete-{suffix}"),
            )
            .unwrap();
        action
    }
}

fn fixture(limits: LocalImportLimits) -> Fixture {
    let sandbox = tempfile::tempdir().unwrap();
    let layout = RuntimePaths::new(sandbox.path().join("runtime"))
        .unwrap()
        .initialize(RuntimeEnvironment::Test)
        .unwrap();
    let database = KernelDatabase::open(&layout).unwrap();
    let jobs = JobStore::open(database.clone(), "generation-local", 64).unwrap();
    let (interactions, host) = open_interaction_broker(database, jobs.clone()).unwrap();
    let artifacts = ArtifactStore::from_layout(
        &layout,
        RuntimeEnvironment::Test,
        "test-target",
        "1",
        TrustStore::default(),
    )
    .unwrap();
    let local = LocalSource::new(
        artifacts.clone(),
        interactions,
        sandbox.path().join("local-staging"),
        limits,
    )
    .unwrap();
    Fixture {
        _sandbox: sandbox,
        local,
        host,
        jobs,
        artifacts,
    }
}

#[test]
fn authorized_file_import_is_immutable_and_content_addressed() {
    let fixture = fixture(LocalImportLimits::default());
    let first = fixture._sandbox.path().join("first.pak");
    let second_dir = fixture._sandbox.path().join("elsewhere");
    fs::create_dir(&second_dir).unwrap();
    let second = second_dir.join("renamed.pak");
    fs::write(&first, b"same pak bytes").unwrap();
    fs::write(&second, b"same pak bytes").unwrap();

    let first_action = fixture.authorize_file("first", &first);
    let first_import = fixture.local.import_file(&first_action, "drg").unwrap();
    let second_action = fixture.authorize_file("second", &second);
    let second_import = fixture.local.import_file(&second_action, "drg").unwrap();
    assert_eq!(first_import.input_kind, LocalInputKind::Pak);
    assert_eq!(first_import.content_kinds, vec![LocalContentKind::Pak]);
    assert_eq!(first_import.artifact.digest, second_import.artifact.digest);
    assert_eq!(
        first_import.mod_ref.remote_file_id,
        Some(first_import.artifact.digest.clone())
    );

    fs::write(&first, b"changed after import").unwrap();
    let grant = fixture
        .artifacts
        .grant_blob(&first_import.artifact)
        .unwrap();
    assert_eq!(fs::read(&grant.descriptor.path).unwrap(), b"same pak bytes");
    assert_eq!(fs::read(&first).unwrap(), b"changed after import");
}

#[test]
fn stale_binding_and_disallowed_extension_cannot_select_another_path() {
    let fixture = fixture(LocalImportLimits::default());
    let pak = fixture._sandbox.path().join("allowed.pak");
    let text = fixture._sandbox.path().join("secret.txt");
    fs::write(&pak, b"pak").unwrap();
    fs::write(&text, b"must remain").unwrap();
    let (job_id, context) = fixture.register("binding");
    let action = fixture
        .local
        .request_file_input(&job_id, &context, "Import")
        .unwrap();
    assert!(fixture
        .host
        .complete_file_selection(
            "owner:test",
            &action.action_id,
            action.revision,
            &text,
            "wrong-extension",
        )
        .is_err());
    fixture
        .host
        .complete_file_selection(
            "owner:test",
            &action.action_id,
            action.revision,
            &pak,
            "right-extension",
        )
        .unwrap();
    let mut tampered = action.clone();
    tampered.target = json!({"purpose": "different", "allowedExtensions": ["pak"], "expiresAtMs": context.expires_at_ms});
    assert!(matches!(
        fixture.local.import_file(&tampered, "drg"),
        Err(LocalSourceError::Broker(BrokerError::Jobs(
            JobError::StalePlan
        )))
    ));
    let imported = fixture.local.import_file(&action, "drg").unwrap();
    assert_eq!(imported.artifact.size, 3);
    assert_eq!(fs::read(&text).unwrap(), b"must remain");
}

#[test]
fn directory_snapshots_are_deterministic_classified_and_detached_from_sources() {
    let fixture = fixture(LocalImportLimits::default());
    let first = fixture._sandbox.path().join("Directory One");
    let second = fixture._sandbox.path().join("Directory Two");
    for root in [&first, &second] {
        fs::create_dir_all(root.join("Content/Mods")).unwrap();
        fs::create_dir_all(root.join("Scripts")).unwrap();
        fs::create_dir_all(root.join("Binaries")).unwrap();
        fs::write(root.join("Content/Mods/Test.uasset"), b"asset").unwrap();
        fs::write(root.join("Scripts/main.js"), b"script").unwrap();
        fs::write(root.join("Binaries/plugin.dll"), b"native").unwrap();
    }
    let first_action = fixture.authorize_directory("directory-one", &first);
    let first_import = fixture
        .local
        .import_directory(&first_action, "drg")
        .unwrap();
    let second_action = fixture.authorize_directory("directory-two", &second);
    let second_import = fixture
        .local
        .import_directory(&second_action, "drg")
        .unwrap();
    assert_eq!(first_import.input_kind, LocalInputKind::UnpackedDirectory);
    assert_eq!(first_import.artifact.digest, second_import.artifact.digest);
    assert_eq!(
        first_import.content_kinds,
        vec![
            LocalContentKind::UnrealAssets,
            LocalContentKind::JavaScript,
            LocalContentKind::NativeBinary
        ]
    );
    assert_eq!(first_import.risk.script_files, 1);
    assert_eq!(first_import.risk.native_binary_files, 1);
    assert!(first_import.risk.requires_approval);

    fs::write(first.join("Content/Mods/Test.uasset"), b"new asset").unwrap();
    let changed_action = fixture.authorize_directory("directory-changed", &first);
    let changed_import = fixture
        .local
        .import_directory(&changed_action, "drg")
        .unwrap();
    assert_ne!(changed_import.artifact.digest, first_import.artifact.digest);
    assert_eq!(
        fs::read(first.join("Content/Mods/Test.uasset")).unwrap(),
        b"new asset"
    );
}

#[cfg(unix)]
#[test]
fn directory_symlink_escape_is_rejected_without_deleting_originals() {
    use std::os::unix::fs::symlink;

    let fixture = fixture(LocalImportLimits::default());
    let root = fixture._sandbox.path().join("linked");
    let outside = fixture._sandbox.path().join("outside.dll");
    fs::create_dir(&root).unwrap();
    fs::write(&outside, b"outside").unwrap();
    symlink(&outside, root.join("escape.dll")).unwrap();
    let action = fixture.authorize_directory("symlink", &root);
    assert!(matches!(
        fixture.local.import_directory(&action, "drg"),
        Err(LocalSourceError::Broker(BrokerError::InteractionMismatch(message)))
            if message.contains("symbolic link")
    ));
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
    assert!(root.join("escape.dll").exists());
}

#[test]
fn directory_and_archive_resource_limits_fail_closed() {
    let limits = LocalImportLimits {
        max_input_bytes: 1024 * 1024,
        max_entries: 2,
        max_entry_bytes: 1024 * 1024,
        max_expanded_bytes: 1024 * 1024,
        max_compression_ratio: 2,
        max_path_depth: 8,
    };
    let fixture = fixture(limits);
    let root = fixture._sandbox.path().join("too-many");
    fs::create_dir_all(root.join("Content")).unwrap();
    for index in 0..3 {
        fs::write(root.join(format!("Content/{index}.uasset")), b"asset").unwrap();
    }
    let directory_action = fixture.authorize_directory("too-many", &root);
    assert!(matches!(
        fixture.local.import_directory(&directory_action, "drg"),
        Err(LocalSourceError::Broker(BrokerError::InputLimitExceeded(limit)))
            if limit == "entry count"
    ));
    assert_eq!(fs::read_dir(root.join("Content")).unwrap().count(), 3);

    let bomb = fixture._sandbox.path().join("bomb.zip");
    write_zip(&bomb, &[("Content/Bomb.uasset", vec![0_u8; 128 * 1024])]);
    let bomb_action = fixture.authorize_file("bomb", &bomb);
    assert!(matches!(
        fixture.local.import_file(&bomb_action, "drg"),
        Err(LocalSourceError::ResourceLimit(limit)) if limit == "archive compression ratio"
    ));
    assert!(bomb.exists());
}

#[test]
fn malicious_archive_paths_and_collisions_never_touch_the_source() {
    let fixture = fixture(LocalImportLimits::default());
    for (suffix, entries) in [
        ("traversal", vec![("../escape.dll", b"x".to_vec())]),
        ("drive", vec![("C:/escape.dll", b"x".to_vec())]),
        ("ads", vec![("Content/file.uasset:stream", b"x".to_vec())]),
        (
            "case-collision",
            vec![
                ("Content/Test.uasset", b"one".to_vec()),
                ("content/test.uasset", b"two".to_vec()),
            ],
        ),
    ] {
        let archive = fixture._sandbox.path().join(format!("{suffix}.zip"));
        write_zip(&archive, &entries);
        let before = fs::read(&archive).unwrap();
        let action = fixture.authorize_file(suffix, &archive);
        assert!(matches!(
            fixture.local.import_file(&action, "drg"),
            Err(LocalSourceError::UnsafePath(_))
        ));
        assert_eq!(fs::read(&archive).unwrap(), before);
        assert!(!fixture._sandbox.path().join("escape.dll").exists());
    }
}

#[test]
fn valid_zip_is_fully_read_and_executable_content_requires_approval() {
    let fixture = fixture(LocalImportLimits::default());
    let archive = fixture._sandbox.path().join("mixed.zip");
    write_zip(
        &archive,
        &[
            ("Content/Test.uasset", b"asset".to_vec()),
            ("Scripts/main.js", b"script".to_vec()),
            ("Binaries/mod.dll", b"native".to_vec()),
        ],
    );
    let action = fixture.authorize_file("mixed", &archive);
    let imported = fixture.local.import_file(&action, "drg").unwrap();
    assert_eq!(imported.input_kind, LocalInputKind::ZipArchive);
    assert_eq!(imported.entry_count, 3);
    assert_eq!(imported.expanded_bytes, 17);
    assert_eq!(imported.risk.script_files, 1);
    assert_eq!(imported.risk.native_binary_files, 1);
    assert!(imported.risk.requires_approval);
}

fn write_zip(path: &std::path::Path, entries: &[(&str, Vec<u8>)]) {
    let file = File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for (name, bytes) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}
