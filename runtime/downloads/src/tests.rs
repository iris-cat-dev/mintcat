use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Cursor,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use async_trait::async_trait;
use mintcat_library_service::{
    AppliedArtifact, ContentRefRecord, DownloadOperationRecord, GameDefinition, LibraryBackup,
    LibraryMod, LibrarySnapshot, ModContentVersion, ProfileContentBindingRecord, ProfileModRecord,
    ProfileRecord, StartDownloadOperation, StartOperationItem, CONTENT_WRITE_PERMISSION,
    LIBRARY_BACKUP_FORMAT, METADATA_WRITE_PERMISSION, PROFILE_WRITE_PERMISSION, READ_PERMISSION,
};
use mintcat_runtime_artifacts::TrustStore;
use mintcat_runtime_broker::{
    interaction_binding, CredentialVault, MemoryCredentialStore, NetworkBroker, Origin,
};
use mintcat_runtime_jobs::{
    request_hash, ActionSpec, ConfirmationPolicy, EventUpdate, JobStore, RecoveryDisposition,
    RegisterJob, ResumePolicy, WorkerCheckpoint,
};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
use mintcat_runtime_transfers::{TransferConfig, TransferManager};
use mintcat_sdk::{
    ActionRequired, DownloadMode, JobState, ModRef, ModsSetVersionParams, ProfilesDeleteParams,
    VersionSelection, VersionSelectionKind,
};
use mintcat_source_api::{
    DownloadDescriptor, DownloadResolution, SearchPage, SearchRequest, SourceDependency,
    SourceError, SourceGameBinding, SourceMod, SourceProvider, SourceVersion,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
    time::{sleep, timeout, Duration},
};
use url::Url;

use super::*;

const OWNER: &str = "test-owner";
const MODULE_DIGEST: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[derive(Clone)]
struct ScriptedResponse {
    status: &'static str,
    body: Vec<u8>,
    sent_first: Option<Arc<Notify>>,
    release: Option<Arc<Notify>>,
}

impl ScriptedResponse {
    fn ok(body: &[u8]) -> Self {
        Self {
            status: "200 OK",
            body: body.to_vec(),
            sent_first: None,
            release: None,
        }
    }

    fn failure() -> Self {
        Self {
            status: "500 Internal Server Error",
            body: b"error".to_vec(),
            sent_first: None,
            release: None,
        }
    }

    fn gated(body: &[u8], sent_first: Arc<Notify>, release: Arc<Notify>) -> Self {
        Self {
            status: "200 OK",
            body: body.to_vec(),
            sent_first: Some(sent_first),
            release: Some(release),
        }
    }
}

async fn scripted_server(
    responses: Vec<ScriptedResponse>,
) -> (Url, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buffer = [0_u8; 2048];
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                raw.extend_from_slice(&buffer[..read]);
                if raw.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(String::from_utf8(raw).unwrap());
            let headers = format!(
                "HTTP/1.1 {}\r\nContent-Length: {}\r\nETag: \"fixture\"\r\nConnection: close\r\n\r\n",
                response.status,
                response.body.len()
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            if let Some(first) = response.body.first() {
                let _ = socket.write_all(&[*first]).await;
            }
            if let Some(sent_first) = response.sent_first {
                sent_first.notify_one();
            }
            if let Some(release) = response.release {
                release.notified().await;
            }
            if response.body.len() > 1 {
                let _ = socket.write_all(&response.body[1..]).await;
            }
        }
        requests
    });
    (base, server)
}

#[derive(Clone)]
struct FixtureProvider {
    descriptors: Arc<HashMap<String, DownloadDescriptor>>,
    resolve_count: Arc<AtomicUsize>,
    interactive_jobs: Option<JobStore>,
}

impl FixtureProvider {
    fn direct(descriptors: HashMap<String, DownloadDescriptor>) -> Self {
        Self {
            descriptors: Arc::new(descriptors),
            resolve_count: Arc::new(AtomicUsize::new(0)),
            interactive_jobs: None,
        }
    }

    fn interactive(descriptors: HashMap<String, DownloadDescriptor>, jobs: JobStore) -> Self {
        Self {
            descriptors: Arc::new(descriptors),
            resolve_count: Arc::new(AtomicUsize::new(0)),
            interactive_jobs: Some(jobs),
        }
    }

    fn descriptor(&self, mod_ref: &ModRef) -> Result<DownloadDescriptor, SourceError> {
        self.resolve_count.fetch_add(1, Ordering::SeqCst);
        self.descriptors
            .get(&mod_ref.remote_mod_id)
            .cloned()
            .ok_or_else(|| SourceError::NotFound(mod_ref.remote_mod_id.clone()))
    }
}

#[async_trait]
impl SourceProvider for FixtureProvider {
    fn source_id(&self) -> &'static str {
        "fixture"
    }

    fn parse_url(
        &self,
        _game: &SourceGameBinding,
        _url: &Url,
    ) -> Result<Option<ModRef>, SourceError> {
        Ok(None)
    }

    async fn search(
        &self,
        _game: &SourceGameBinding,
        _request: &SearchRequest,
        _cancellation: &CancellationToken,
    ) -> Result<SearchPage, SourceError> {
        Err(SourceError::Unavailable("not used by fixture".to_owned()))
    }

    async fn get_mod(
        &self,
        _game: &SourceGameBinding,
        _mod_ref: &ModRef,
        _cancellation: &CancellationToken,
    ) -> Result<SourceMod, SourceError> {
        Err(SourceError::Unavailable("not used by fixture".to_owned()))
    }

    async fn list_versions(
        &self,
        _game: &SourceGameBinding,
        _mod_ref: &ModRef,
        _cancellation: &CancellationToken,
    ) -> Result<Vec<SourceVersion>, SourceError> {
        Err(SourceError::Unavailable("not used by fixture".to_owned()))
    }

    async fn get_dependencies(
        &self,
        _game: &SourceGameBinding,
        _mod_ref: &ModRef,
        _cancellation: &CancellationToken,
    ) -> Result<Vec<SourceDependency>, SourceError> {
        Err(SourceError::Unavailable("not used by fixture".to_owned()))
    }

    async fn resolve_download(
        &self,
        _game: &SourceGameBinding,
        mod_ref: &ModRef,
        _cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        self.descriptor(mod_ref)
    }

    async fn begin_download(
        &self,
        job_id: &str,
        context: &InteractionContext,
        _game: &SourceGameBinding,
        mod_ref: &ModRef,
        _cancellation: &CancellationToken,
    ) -> Result<DownloadResolution, SourceError> {
        let Some(jobs) = &self.interactive_jobs else {
            return self.descriptor(mod_ref).map(DownloadResolution::Ready);
        };
        let action = jobs
            .create_action(
                job_id,
                ActionSpec {
                    kind: "fixture-download".to_owned(),
                    operation_id: context.operation_id.clone(),
                    target: json!({"remoteModId": mod_ref.remote_mod_id}),
                    target_revision: context.target_revision,
                    plan_hash: context.plan_hash.clone(),
                    module_lock_digest: context.module_lock_digest.clone(),
                    expires_at_ms: context.expires_at_ms,
                    response_schema: json!({
                        "type": "object",
                        "additionalProperties": false
                    }),
                },
            )
            .map_err(|error| SourceError::InvalidResponse(error.to_string()))?;
        Ok(DownloadResolution::InteractionRequired(action))
    }

    async fn resume_download_action(
        &self,
        action: &ActionRequired,
        _game: &SourceGameBinding,
        mod_ref: &ModRef,
        _cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        let jobs = self.interactive_jobs.as_ref().ok_or_else(|| {
            SourceError::InvalidArgument("fixture interaction is disabled".to_owned())
        })?;
        jobs.consume_action(&action.action_id, &interaction_binding(action))
            .map_err(|error| SourceError::InvalidResponse(error.to_string()))?;
        self.descriptor(mod_ref)
    }
}

struct Fixture {
    _sandbox: TempDir,
    library: LibraryStore,
    authorization: LibraryAuthorization,
    jobs: JobStore,
    artifacts: ArtifactStore,
    coordinator: DownloadCoordinator,
    catalog: DownloadCatalog,
    old_one_digest: String,
    old_two_digest: String,
}

fn fixture() -> Fixture {
    let sandbox = tempfile::tempdir().unwrap();
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
    let old_one = artifacts
        .publish_blob_reader(&mut Cursor::new(b"old-one"), None)
        .unwrap();
    let old_two = artifacts
        .publish_blob_reader(&mut Cursor::new(b"old-two"), None)
        .unwrap();
    let library = LibraryStore::open(&layout).unwrap();
    let backup_path = sandbox.path().join("seed.json");
    fs::write(
        &backup_path,
        serde_json::to_vec(&LibraryBackup {
            format: LIBRARY_BACKUP_FORMAT.to_owned(),
            exported_at_ms: 1,
            snapshot: seed_snapshot(&old_one, &old_two),
        })
        .unwrap(),
    )
    .unwrap();
    library.restore_from(&backup_path).unwrap();

    let database = KernelDatabase::open(&layout).unwrap();
    let jobs = JobStore::open(database.clone(), "generation-test", 256).unwrap();
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let broker = NetworkBroker::new(&vault, None).unwrap();
    let transfers = TransferManager::open(
        database,
        artifacts.clone(),
        broker,
        sandbox.path().join("transfer-staging"),
        TransferConfig {
            max_bytes: 1024 * 1024,
            progress_step_bytes: 1,
        },
    )
    .unwrap();
    let authorization = LibraryAuthorization::new(
        OWNER,
        [
            READ_PERMISSION,
            PROFILE_WRITE_PERMISSION,
            CONTENT_WRITE_PERMISSION,
            METADATA_WRITE_PERMISSION,
        ],
    );
    let catalog = DownloadCatalog::default();
    let coordinator = DownloadCoordinator::new(
        library.clone(),
        authorization.clone(),
        jobs.clone(),
        transfers,
        artifacts.clone(),
        catalog.clone(),
    );
    Fixture {
        _sandbox: sandbox,
        library,
        authorization,
        jobs,
        artifacts,
        coordinator,
        catalog,
        old_one_digest: old_one.digest,
        old_two_digest: old_two.digest,
    }
}

fn seed_snapshot(
    old_one: &mintcat_sdk::ArtifactRef,
    old_two: &mintcat_sdk::ArtifactRef,
) -> LibrarySnapshot {
    let profiles = ["profile-a", "profile-b"]
        .into_iter()
        .map(|profile_id| ProfileRecord {
            profile_id: profile_id.to_owned(),
            game_id: "game".to_owned(),
            owner_scope: OWNER.to_owned(),
            name: profile_id.to_owned(),
            display_name: profile_id.to_owned(),
            active: profile_id == "profile-a",
            description: String::new(),
            revision: 1,
            content_revision: 1,
            lease_epoch: 0,
            incarnation_id: format!("incarnation:{profile_id}"),
            last_used_at_ms: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted_at_ms: None,
        })
        .collect::<Vec<_>>();
    let mods = ["mod-1", "mod-2"]
        .into_iter()
        .map(|mod_id| LibraryMod {
            mod_id: mod_id.to_owned(),
            game_id: "game".to_owned(),
            source_id: "fixture".to_owned(),
            remote_game_key: "remote-game".to_owned(),
            remote_mod_id: mod_id.to_owned(),
            revision: 1,
            incarnation_id: format!("incarnation:{mod_id}"),
            name_id: mod_id.to_owned(),
            display_name: mod_id.to_owned(),
            original_name: mod_id.to_owned(),
            url: String::new(),
            tags: Vec::new(),
            approval_status: "accepted".to_owned(),
            dependency_mod_id: None,
            last_update_at_ms: 1,
            online_update_at_ms: 1,
            online_available: true,
            local_missing: false,
            created_at_ms: 1,
            updated_at_ms: 1,
        })
        .collect::<Vec<_>>();
    let versions = [
        ("mod-1", "v1", "file-1", Some(old_one), false),
        ("mod-1", "v2", "file-2", None, true),
        ("mod-2", "v1", "file-1", Some(old_two), false),
        ("mod-2", "v2", "file-2", None, true),
    ]
    .into_iter()
    .map(
        |(mod_id, version_id, file_id, artifact, current)| ModContentVersion {
            mod_id: mod_id.to_owned(),
            version_id: version_id.to_owned(),
            remote_file_id: Some(file_id.to_owned()),
            artifact_digest: artifact.map(|value| value.digest.clone()),
            legacy_location: None,
            availability: "remote".to_owned(),
            current,
            created_at_ms: 1,
            updated_at_ms: 1,
            source_url: None,
            file_size: artifact.map_or(0, |value| value.size),
            legacy_download_status: None,
        },
    )
    .collect::<Vec<_>>();
    let profile_mods = [
        ("pm-a-1", "profile-a", "mod-1"),
        ("pm-a-2", "profile-a", "mod-2"),
        ("pm-b-1", "profile-b", "mod-1"),
    ]
    .into_iter()
    .map(|(profile_mod_id, profile_id, mod_id)| ProfileModRecord {
        profile_mod_id: profile_mod_id.to_owned(),
        profile_id: profile_id.to_owned(),
        mod_id: mod_id.to_owned(),
        group_id: None,
        incarnation_id: format!("incarnation:{profile_mod_id}"),
        sort_order: 0,
        enabled: true,
        used_version: Some("v1".to_owned()),
        version_selection_kind: "pinned".to_owned(),
        selected_version_id: Some("v1".to_owned()),
        created_at_ms: 1,
        updated_at_ms: 1,
    })
    .collect::<Vec<_>>();
    let old_operation = DownloadOperationRecord {
        operation_id: "old-operation".to_owned(),
        owner_scope: OWNER.to_owned(),
        idempotency_scope: "seed".to_owned(),
        idempotency_key: "seed".to_owned(),
        inputs_hash: MODULE_DIGEST.to_owned(),
        profile_id: "profile-a".to_owned(),
        mode: "prepare-and-apply".to_owned(),
        expected_content_revision: 0,
        target_incarnation_id: "incarnation:profile-a".to_owned(),
        owner_generation: "seed-generation".to_owned(),
        fence_epoch: 0,
        state: "succeeded".to_owned(),
        result_json: Some("{}".to_owned()),
        error_code: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: Some(1),
    };
    LibrarySnapshot {
        games: vec![GameDefinition {
            game_id: "game".to_owned(),
            game_key: "game".to_owned(),
            display_name: "Game".to_owned(),
            icon: String::new(),
            active: true,
            created_at_ms: 1,
            updated_at_ms: 1,
        }],
        profiles,
        mods,
        versions,
        profile_mods,
        download_operations: vec![old_operation],
        content_refs: vec![
            ContentRefRecord {
                digest: old_one.digest.clone(),
                size: old_one.size,
                verified: true,
                grant_id: "seed-one".to_owned(),
                created_at_ms: 1,
            },
            ContentRefRecord {
                digest: old_two.digest.clone(),
                size: old_two.size,
                verified: true,
                grant_id: "seed-two".to_owned(),
                created_at_ms: 1,
            },
        ],
        profile_content_bindings: vec![
            ProfileContentBindingRecord {
                profile_mod_id: "pm-a-1".to_owned(),
                version_id: "v1".to_owned(),
                artifact_digest: old_one.digest.clone(),
                operation_id: "old-operation".to_owned(),
                updated_at_ms: 1,
            },
            ProfileContentBindingRecord {
                profile_mod_id: "pm-a-2".to_owned(),
                version_id: "v1".to_owned(),
                artifact_digest: old_two.digest.clone(),
                operation_id: "old-operation".to_owned(),
                updated_at_ms: 1,
            },
            ProfileContentBindingRecord {
                profile_mod_id: "pm-b-1".to_owned(),
                version_id: "v1".to_owned(),
                artifact_digest: old_one.digest.clone(),
                operation_id: "old-operation".to_owned(),
                updated_at_ms: 1,
            },
        ],
        ..LibrarySnapshot::default()
    }
}

fn descriptor(base: &Url, mod_id: &str, body_size: usize) -> DownloadDescriptor {
    let url = base.join(&format!("/{mod_id}.bin")).unwrap();
    DownloadDescriptor {
        mod_ref: ModRef {
            source_id: "fixture".to_owned(),
            remote_game_key: "remote-game".to_owned(),
            remote_mod_id: mod_id.to_owned(),
            version_id: Some("v2".to_owned()),
            remote_file_id: Some("file-2".to_owned()),
        },
        url: url.clone(),
        allowed_origins: vec![Origin::from_url(&url).unwrap()],
        headers: BTreeMap::new(),
        credential: None,
        expires_at: None,
        expected_size: body_size as u64,
        expected_md5: None,
    }
}

fn install_provider(fixture: &Fixture, provider: FixtureProvider) -> Arc<FixtureProvider> {
    let provider = Arc::new(provider);
    fixture
        .catalog
        .register_provider(provider.clone(), MODULE_DIGEST)
        .unwrap();
    fixture
        .catalog
        .register_game(
            "fixture",
            SourceGameBinding {
                game_id: "game".to_owned(),
                remote_game_key: "remote-game".to_owned(),
                provider_game_id: "fixture-game".to_owned(),
            },
        )
        .unwrap();
    provider
}

fn start_version(
    fixture: &Fixture,
    profile_id: &str,
    profile_mod_id: &str,
    mode: DownloadMode,
    idempotency_key: &str,
) -> mintcat_library_service::StartedDownloadOperation {
    let snapshot = fixture.library.snapshot().unwrap();
    let profile = snapshot
        .profiles
        .iter()
        .find(|profile| profile.profile_id == profile_id)
        .unwrap();
    let profile_mod = snapshot
        .profile_mods
        .iter()
        .find(|profile_mod| profile_mod.profile_mod_id == profile_mod_id)
        .unwrap();
    fixture
        .library
        .start_version_download(
            &fixture.authorization,
            idempotency_key,
            "generation-test",
            &format!("sha256:{}", "a".repeat(64)),
            &ModsSetVersionParams {
                profile_id: profile_id.to_owned(),
                profile_mod_id: profile_mod_id.to_owned(),
                incarnation_id: profile_mod.incarnation_id.clone(),
                expected_content_revision: profile.content_revision,
                selection: VersionSelection {
                    kind: VersionSelectionKind::Pinned,
                    version_id: Some("v2".to_owned()),
                },
                mode,
            },
        )
        .unwrap()
}

fn register_job(
    fixture: &Fixture,
    operation: &mintcat_library_service::DownloadOperationSnapshot,
) -> String {
    fixture
        .jobs
        .register_job(RegisterJob {
            command: "library.download.apply".to_owned(),
            owner_scope: OWNER.to_owned(),
            idempotency_key: operation.operation_id.clone(),
            request_hash: operation.inputs_hash.clone(),
            target: json!({"operationId": operation.operation_id}),
            confirmation_policy: ConfirmationPolicy::Policy,
        })
        .unwrap()
        .job
        .job_id
}

fn digest(body: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(body))
}

#[tokio::test]
async fn prepare_and_apply_updates_selection_binding_and_job_atomically() {
    let fixture = fixture();
    let body = b"new-version-one";
    let (base, server) = scripted_server(vec![ScriptedResponse::ok(body)]).await;
    install_provider(
        &fixture,
        FixtureProvider::direct(HashMap::from([(
            "mod-1".to_owned(),
            descriptor(&base, "mod-1", body.len()),
        )])),
    );
    let started = start_version(
        &fixture,
        "profile-a",
        "pm-a-1",
        DownloadMode::PrepareAndApply,
        "apply-v2",
    );
    let job_id = register_job(&fixture, &started.operation);

    let outcome = fixture
        .coordinator
        .execute(&job_id, &started.operation.operation_id, None)
        .await
        .unwrap();
    let DownloadExecutionResult::Succeeded { result } = outcome else {
        panic!("expected successful apply");
    };
    assert!(result.applied);
    assert_eq!(result.content_revision, 2);
    assert_eq!(result.artifact_digests, vec![digest(body)]);

    let snapshot = fixture.library.snapshot().unwrap();
    let profile_mod = snapshot
        .profile_mods
        .iter()
        .find(|record| record.profile_mod_id == "pm-a-1")
        .unwrap();
    assert_eq!(profile_mod.used_version.as_deref(), Some("v2"));
    assert_eq!(profile_mod.version_selection_kind, "pinned");
    assert_eq!(profile_mod.selected_version_id.as_deref(), Some("v2"));
    let binding = snapshot
        .profile_content_bindings
        .iter()
        .find(|record| record.profile_mod_id == "pm-a-1")
        .unwrap();
    assert_eq!(binding.version_id, "v2");
    assert_eq!(binding.artifact_digest, digest(body));
    assert_eq!(
        fixture.jobs.get_job(OWNER, &job_id).unwrap().state,
        JobState::Succeeded
    );
    assert_eq!(server.await.unwrap().len(), 1);
    assert!(fixture
        .library
        .retained_artifact_digests(&fixture.authorization)
        .unwrap()
        .contains(&digest(body)));
    let replay = fixture
        .library
        .start_version_download(
            &fixture.authorization,
            "apply-v2",
            "generation-after-commit",
            &format!("sha256:{}", "a".repeat(64)),
            &ModsSetVersionParams {
                profile_id: "profile-a".to_owned(),
                profile_mod_id: "pm-a-1".to_owned(),
                incarnation_id: "incarnation:pm-a-1".to_owned(),
                expected_content_revision: 1,
                selection: VersionSelection {
                    kind: VersionSelectionKind::Pinned,
                    version_id: Some("v2".to_owned()),
                },
                mode: DownloadMode::PrepareAndApply,
            },
        )
        .unwrap();
    assert!(!replay.created);
    assert_eq!(
        replay.operation.operation_id,
        started.operation.operation_id
    );
}

#[tokio::test]
async fn cache_only_survives_profile_deletion_without_changing_bindings() {
    let fixture = fixture();
    let body = b"cache-only-version";
    let (base, server) = scripted_server(vec![ScriptedResponse::ok(body)]).await;
    install_provider(
        &fixture,
        FixtureProvider::direct(HashMap::from([(
            "mod-1".to_owned(),
            descriptor(&base, "mod-1", body.len()),
        )])),
    );
    let started = start_version(
        &fixture,
        "profile-a",
        "pm-a-1",
        DownloadMode::CacheOnly,
        "cache-v2",
    );
    let job_id = register_job(&fixture, &started.operation);
    fixture
        .library
        .delete_profile_command(
            &fixture.authorization,
            "delete-profile-a",
            &ProfilesDeleteParams {
                profile_id: "profile-a".to_owned(),
                expected_revision: 1,
            },
        )
        .unwrap();

    let outcome = fixture
        .coordinator
        .execute(&job_id, &started.operation.operation_id, None)
        .await
        .unwrap();
    let DownloadExecutionResult::Succeeded { result } = outcome else {
        panic!("expected successful cache fill");
    };
    assert!(!result.applied);
    let snapshot = fixture.library.snapshot().unwrap();
    assert!(snapshot
        .profiles
        .iter()
        .find(|profile| profile.profile_id == "profile-a")
        .is_some_and(|profile| profile.deleted_at_ms.is_some()));
    assert!(!snapshot
        .profile_content_bindings
        .iter()
        .any(|binding| binding.profile_mod_id == "pm-a-1"));
    let version = snapshot
        .versions
        .iter()
        .find(|version| version.mod_id == "mod-1" && version.version_id == "v2")
        .unwrap();
    assert_eq!(
        version.artifact_digest.as_deref(),
        Some(digest(body).as_str())
    );
    assert_eq!(server.await.unwrap().len(), 1);
    let replay = fixture
        .library
        .start_version_download(
            &fixture.authorization,
            "cache-v2",
            "generation-after-delete",
            &format!("sha256:{}", "a".repeat(64)),
            &ModsSetVersionParams {
                profile_id: "profile-a".to_owned(),
                profile_mod_id: "pm-a-1".to_owned(),
                incarnation_id: "incarnation:pm-a-1".to_owned(),
                expected_content_revision: 1,
                selection: VersionSelection {
                    kind: VersionSelectionKind::Pinned,
                    version_id: Some("v2".to_owned()),
                },
                mode: DownloadMode::CacheOnly,
            },
        )
        .unwrap();
    assert!(!replay.created);
    assert_eq!(
        replay.operation.operation_id,
        started.operation.operation_id
    );
}

#[tokio::test]
async fn batch_failure_caches_verified_items_but_preserves_old_profile_bindings() {
    let fixture = fixture();
    let first_body = b"batch-first";
    let second_body = b"batch-second";
    let (first_base, first_server) = scripted_server(vec![ScriptedResponse::ok(first_body)]).await;
    let (second_base, second_server) = scripted_server(vec![ScriptedResponse::failure()]).await;
    install_provider(
        &fixture,
        FixtureProvider::direct(HashMap::from([
            (
                "mod-1".to_owned(),
                descriptor(&first_base, "mod-1", first_body.len()),
            ),
            (
                "mod-2".to_owned(),
                descriptor(&second_base, "mod-2", second_body.len()),
            ),
        ])),
    );
    let start_item =
        |item_id: &str, profile_mod_id: &str, mod_id: &str, expected: String| StartOperationItem {
            item_id: item_id.to_owned(),
            profile_mod_id: profile_mod_id.to_owned(),
            profile_mod_incarnation_id: format!("incarnation:{profile_mod_id}"),
            mod_id: mod_id.to_owned(),
            source_id: "fixture".to_owned(),
            source_mod_id: mod_id.to_owned(),
            remote_game_key: "remote-game".to_owned(),
            source_file_id: Some("file-2".to_owned()),
            resolved_version: "v2".to_owned(),
            expected_digest: Some(expected),
            validator: None,
            selection: Some(VersionSelection {
                kind: VersionSelectionKind::Pinned,
                version_id: Some("v2".to_owned()),
            }),
        };
    let started = fixture
        .library
        .start_download_operation(
            &fixture.authorization,
            &StartDownloadOperation {
                idempotency_scope: "batch".to_owned(),
                idempotency_key: "batch-failure".to_owned(),
                profile_id: "profile-a".to_owned(),
                expected_content_revision: 1,
                target_incarnation_id: "incarnation:profile-a".to_owned(),
                owner_generation: "generation-test".to_owned(),
                module_lock_digest: format!("sha256:{}", "a".repeat(64)),
                mode: DownloadMode::PrepareAndApply,
                items: vec![
                    start_item("first", "pm-a-1", "mod-1", digest(first_body)),
                    start_item("second", "pm-a-2", "mod-2", digest(second_body)),
                ],
            },
        )
        .unwrap();
    let job_id = register_job(&fixture, &started.operation);

    assert!(fixture
        .coordinator
        .execute(&job_id, &started.operation.operation_id, None)
        .await
        .is_err());
    assert_eq!(first_server.await.unwrap().len(), 1);
    assert_eq!(second_server.await.unwrap().len(), 1);

    let operation = fixture
        .library
        .download_operation(&fixture.authorization, &started.operation.operation_id)
        .unwrap();
    assert_eq!(operation.state, DownloadOperationState::Failed);
    assert_eq!(
        fixture.jobs.get_job(OWNER, &job_id).unwrap().state,
        JobState::Failed
    );
    let snapshot = fixture.library.snapshot().unwrap();
    let profile = snapshot
        .profiles
        .iter()
        .find(|profile| profile.profile_id == "profile-a")
        .unwrap();
    assert_eq!(profile.content_revision, 1);
    for (profile_mod_id, old_digest) in [
        ("pm-a-1", fixture.old_one_digest.as_str()),
        ("pm-a-2", fixture.old_two_digest.as_str()),
    ] {
        let profile_mod = snapshot
            .profile_mods
            .iter()
            .find(|record| record.profile_mod_id == profile_mod_id)
            .unwrap();
        assert_eq!(profile_mod.used_version.as_deref(), Some("v1"));
        assert_eq!(profile_mod.selected_version_id.as_deref(), Some("v1"));
        let binding = snapshot
            .profile_content_bindings
            .iter()
            .find(|record| record.profile_mod_id == profile_mod_id)
            .unwrap();
        assert_eq!(binding.version_id, "v1");
        assert_eq!(binding.artifact_digest, old_digest);
    }
    let first_version = snapshot
        .versions
        .iter()
        .find(|version| version.mod_id == "mod-1" && version.version_id == "v2")
        .unwrap();
    assert_eq!(
        first_version.artifact_digest.as_deref(),
        Some(digest(first_body).as_str())
    );
    let second_version = snapshot
        .versions
        .iter()
        .find(|version| version.mod_id == "mod-2" && version.version_id == "v2")
        .unwrap();
    assert_eq!(second_version.artifact_digest, None);
}

#[tokio::test]
async fn one_transfer_serves_simultaneous_profile_operations() {
    let fixture = fixture();
    let body = b"shared-profile-version";
    let (base, server) = scripted_server(vec![ScriptedResponse::ok(body)]).await;
    let provider = install_provider(
        &fixture,
        FixtureProvider::direct(HashMap::from([(
            "mod-1".to_owned(),
            descriptor(&base, "mod-1", body.len()),
        )])),
    );
    let first = start_version(
        &fixture,
        "profile-a",
        "pm-a-1",
        DownloadMode::PrepareAndApply,
        "simultaneous-a",
    );
    let second = start_version(
        &fixture,
        "profile-b",
        "pm-b-1",
        DownloadMode::PrepareAndApply,
        "simultaneous-b",
    );
    let first_job = register_job(&fixture, &first.operation);
    let second_job = register_job(&fixture, &second.operation);

    let (first_result, second_result) = tokio::join!(
        fixture
            .coordinator
            .execute(&first_job, &first.operation.operation_id, None),
        fixture
            .coordinator
            .execute(&second_job, &second.operation.operation_id, None)
    );
    assert!(matches!(
        first_result.unwrap(),
        DownloadExecutionResult::Succeeded { .. }
    ));
    assert!(matches!(
        second_result.unwrap(),
        DownloadExecutionResult::Succeeded { .. }
    ));
    assert_eq!(server.await.unwrap().len(), 1);
    assert_eq!(provider.resolve_count.load(Ordering::SeqCst), 2);
    let snapshot = fixture.library.snapshot().unwrap();
    for profile_mod_id in ["pm-a-1", "pm-b-1"] {
        let binding = snapshot
            .profile_content_bindings
            .iter()
            .find(|binding| binding.profile_mod_id == profile_mod_id)
            .unwrap();
        assert_eq!(binding.version_id, "v2");
        assert_eq!(binding.artifact_digest, digest(body));
    }
}

#[tokio::test]
async fn approved_interaction_resumes_the_same_fixed_plan() {
    let fixture = fixture();
    let body = b"interaction-version";
    let (base, server) = scripted_server(vec![ScriptedResponse::ok(body)]).await;
    install_provider(
        &fixture,
        FixtureProvider::interactive(
            HashMap::from([("mod-1".to_owned(), descriptor(&base, "mod-1", body.len()))]),
            fixture.jobs.clone(),
        ),
    );
    let started = start_version(
        &fixture,
        "profile-a",
        "pm-a-1",
        DownloadMode::PrepareAndApply,
        "interaction",
    );
    let job_id = register_job(&fixture, &started.operation);
    let first = fixture
        .coordinator
        .execute(&job_id, &started.operation.operation_id, None)
        .await
        .unwrap();
    let DownloadExecutionResult::AwaitingAction { action } = first else {
        panic!("expected an interaction action");
    };
    assert_eq!(
        fixture.jobs.get_job(OWNER, &job_id).unwrap().state,
        JobState::AwaitingApproval
    );
    fixture
        .jobs
        .approve_action(
            OWNER,
            &action.action_id,
            action.revision,
            &json!({}),
            "approve-interaction",
            &request_hash(&json!({"actionId": action.action_id})).unwrap(),
        )
        .unwrap();
    let approved = fixture.jobs.get_action(OWNER, &action.action_id).unwrap();
    let resumed = fixture
        .coordinator
        .execute(&job_id, &started.operation.operation_id, Some(&approved))
        .await
        .unwrap();
    assert!(matches!(resumed, DownloadExecutionResult::Succeeded { .. }));
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_revokes_the_fence_before_a_late_transfer_can_apply() {
    let fixture = fixture();
    let body = b"cancelled-version";
    let sent_first = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (base, server) = scripted_server(vec![ScriptedResponse::gated(
        body,
        Arc::clone(&sent_first),
        Arc::clone(&release),
    )])
    .await;
    install_provider(
        &fixture,
        FixtureProvider::direct(HashMap::from([(
            "mod-1".to_owned(),
            descriptor(&base, "mod-1", body.len()),
        )])),
    );
    let started = start_version(
        &fixture,
        "profile-a",
        "pm-a-1",
        DownloadMode::PrepareAndApply,
        "cancel-race",
    );
    let job_id = register_job(&fixture, &started.operation);
    let coordinator = fixture.coordinator.clone();
    let execution_job = job_id.clone();
    let operation_id = started.operation.operation_id.clone();
    let execution = tokio::spawn(async move {
        coordinator
            .execute(&execution_job, &operation_id, None)
            .await
    });
    sent_first.notified().await;
    let transfer_progress = timeout(Duration::from_secs(5), async {
        loop {
            if let Some(progress) = fixture
                .coordinator
                .transfer_progress(&started.operation.operation_id, "primary")
                .unwrap()
            {
                if progress
                    .event
                    .as_ref()
                    .is_some_and(|event| event.downloaded_bytes >= 1)
                {
                    break progress;
                }
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("active transfer progress was not observable");
    let transfer_event = transfer_progress.event.unwrap();
    assert_eq!(
        transfer_event.transfer_id,
        transfer_progress.transfer.transfer_id
    );
    assert!(transfer_event.attempt_id.is_some());
    assert_eq!(transfer_event.total_bytes, Some(body.len() as u64));
    let job = fixture.jobs.get_job(OWNER, &job_id).unwrap();
    fixture
        .jobs
        .request_cancel(
            OWNER,
            &job_id,
            job.last_seq,
            "cancel-job",
            &request_hash(&json!({"jobId": job_id})).unwrap(),
        )
        .unwrap();
    fixture
        .coordinator
        .cancel(&job_id, &started.operation.operation_id)
        .unwrap();
    release.notify_one();
    let outcome = execution.await.unwrap().unwrap();
    assert!(matches!(outcome, DownloadExecutionResult::Cancelled { .. }));
    assert_eq!(server.await.unwrap().len(), 1);

    let operation = fixture
        .library
        .download_operation(&fixture.authorization, &started.operation.operation_id)
        .unwrap();
    assert_eq!(operation.state, DownloadOperationState::Cancelled);
    assert_eq!(
        fixture.jobs.get_job(OWNER, &job_id).unwrap().state,
        JobState::Cancelled
    );
    let snapshot = fixture.library.snapshot().unwrap();
    let binding = snapshot
        .profile_content_bindings
        .iter()
        .find(|binding| binding.profile_mod_id == "pm-a-1")
        .unwrap();
    assert_eq!(binding.version_id, "v1");
    assert_eq!(binding.artifact_digest, fixture.old_one_digest);
}

#[tokio::test]
async fn restart_resumes_a_checkpointed_operation_with_a_fresh_fence() {
    let fixture = fixture();
    let body = b"recovered-version";
    let (base, server) = scripted_server(vec![ScriptedResponse::ok(body)]).await;
    install_provider(
        &fixture,
        FixtureProvider::direct(HashMap::from([(
            "mod-1".to_owned(),
            descriptor(&base, "mod-1", body.len()),
        )])),
    );
    let started = start_version(
        &fixture,
        "profile-a",
        "pm-a-1",
        DownloadMode::PrepareAndApply,
        "recovery",
    );
    let job_id = register_job(&fixture, &started.operation);
    let preparing = fixture
        .jobs
        .append_event(
            &job_id,
            EventUpdate::state("download-preparing", JobState::Preparing),
        )
        .unwrap();
    fixture
        .jobs
        .save_checkpoint(
            &job_id,
            preparing.last_seq,
            &WorkerCheckpoint {
                step: "download-operation".to_owned(),
                payload: json!({"operationId": started.operation.operation_id}),
                resume_policy: ResumePolicy::ResumeFromCheckpoint,
            },
        )
        .unwrap();
    fixture
        .library
        .mark_download_running(
            &started.operation.operation_id,
            started.operation.fence.fence_epoch,
            &started.operation.fence.owner_generation,
        )
        .unwrap();

    let restarted_jobs =
        JobStore::open(fixture.jobs.database().clone(), "generation-restarted", 256).unwrap();
    let recovery = restarted_jobs.recover_incomplete().unwrap();
    assert_eq!(recovery.len(), 1);
    assert_eq!(
        recovery[0].disposition,
        RecoveryDisposition::ResumeCheckpoint
    );
    assert_eq!(recovery[0].snapshot.state, JobState::Queued);
    assert_eq!(
        fixture
            .library
            .recover_download_operations("generation-restarted")
            .unwrap(),
        1
    );
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let transfers = TransferManager::open(
        fixture.jobs.database().clone(),
        fixture.artifacts.clone(),
        NetworkBroker::new(&vault, None).unwrap(),
        fixture._sandbox.path().join("transfer-staging"),
        TransferConfig {
            max_bytes: 1024 * 1024,
            progress_step_bytes: 1,
        },
    )
    .unwrap();
    let restarted = DownloadCoordinator::new(
        fixture.library.clone(),
        fixture.authorization.clone(),
        restarted_jobs.clone(),
        transfers,
        fixture.artifacts.clone(),
        fixture.catalog.clone(),
    );

    let outcome = restarted
        .execute(&job_id, &started.operation.operation_id, None)
        .await
        .unwrap();
    assert!(matches!(outcome, DownloadExecutionResult::Succeeded { .. }));
    let recovered_operation = fixture
        .library
        .download_operation(&fixture.authorization, &started.operation.operation_id)
        .unwrap();
    assert_eq!(recovered_operation.state, DownloadOperationState::Succeeded);
    assert!(recovered_operation.fence.fence_epoch > started.operation.fence.fence_epoch);
    assert_eq!(
        recovered_operation.fence.owner_generation,
        "generation-restarted"
    );
    assert_eq!(
        restarted_jobs.get_job(OWNER, &job_id).unwrap().state,
        JobState::Succeeded
    );
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn restart_reconciles_library_commit_without_reapplying_revision() {
    let fixture = fixture();
    let body = b"committed-before-response";
    let artifact = fixture
        .artifacts
        .publish_blob_reader(&mut Cursor::new(body), None)
        .unwrap();
    let started = fixture
        .library
        .start_download_operation(
            &fixture.authorization,
            &StartDownloadOperation {
                idempotency_scope: "commit-recovery".to_owned(),
                idempotency_key: "commit-recovery".to_owned(),
                profile_id: "profile-a".to_owned(),
                expected_content_revision: 1,
                target_incarnation_id: "incarnation:profile-a".to_owned(),
                owner_generation: "generation-test".to_owned(),
                module_lock_digest: format!("sha256:{}", "a".repeat(64)),
                mode: DownloadMode::PrepareAndApply,
                items: vec![StartOperationItem {
                    item_id: "primary".to_owned(),
                    profile_mod_id: "pm-a-1".to_owned(),
                    profile_mod_incarnation_id: "incarnation:pm-a-1".to_owned(),
                    mod_id: "mod-1".to_owned(),
                    source_id: "fixture".to_owned(),
                    source_mod_id: "mod-1".to_owned(),
                    remote_game_key: "remote-game".to_owned(),
                    source_file_id: Some("file-2".to_owned()),
                    resolved_version: "v2".to_owned(),
                    expected_digest: Some(artifact.digest.clone()),
                    validator: None,
                    selection: Some(VersionSelection {
                        kind: VersionSelectionKind::Pinned,
                        version_id: Some("v2".to_owned()),
                    }),
                }],
            },
        )
        .unwrap();
    let job_id = register_job(&fixture, &started.operation);
    fixture
        .jobs
        .append_event(
            &job_id,
            EventUpdate::state("download-preparing", JobState::Preparing),
        )
        .unwrap();
    fixture
        .jobs
        .append_event(
            &job_id,
            EventUpdate::state("download-running", JobState::Running),
        )
        .unwrap();
    fixture
        .library
        .mark_download_running(
            &started.operation.operation_id,
            started.operation.fence.fence_epoch,
            &started.operation.fence.owner_generation,
        )
        .unwrap();
    let grant = fixture.artifacts.grant_blob(&artifact).unwrap();
    fixture
        .library
        .record_download_artifact(
            &fixture.authorization,
            &started.operation.operation_id,
            started.operation.fence.fence_epoch,
            &started.operation.fence.owner_generation,
            "primary",
            &grant,
        )
        .unwrap();
    fixture.jobs.begin_commit(&job_id).unwrap();
    let committed = fixture
        .library
        .apply_download_operation(
            &fixture.authorization,
            &started.operation.operation_id,
            started.operation.fence.fence_epoch,
            &started.operation.fence.owner_generation,
            &[AppliedArtifact {
                item_id: "primary",
                grant: &grant,
            }],
        )
        .unwrap();
    assert_eq!(committed.content_revision, 2);

    let restarted_jobs = JobStore::open(
        fixture.jobs.database().clone(),
        "generation-commit-recovery",
        256,
    )
    .unwrap();
    let recovery = restarted_jobs.recover_incomplete().unwrap();
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0].disposition, RecoveryDisposition::InspectCommit);
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let transfers = TransferManager::open(
        fixture.jobs.database().clone(),
        fixture.artifacts.clone(),
        NetworkBroker::new(&vault, None).unwrap(),
        fixture._sandbox.path().join("transfer-staging"),
        TransferConfig {
            max_bytes: 1024 * 1024,
            progress_step_bytes: 1,
        },
    )
    .unwrap();
    let restarted = DownloadCoordinator::new(
        fixture.library.clone(),
        fixture.authorization.clone(),
        restarted_jobs.clone(),
        transfers,
        fixture.artifacts.clone(),
        fixture.catalog.clone(),
    );
    let outcome = restarted
        .execute(&job_id, &started.operation.operation_id, None)
        .await
        .unwrap();
    let DownloadExecutionResult::Succeeded { result } = outcome else {
        panic!("expected recovered committed operation");
    };
    assert_eq!(result.content_revision, 2);
    assert_eq!(
        fixture
            .library
            .snapshot()
            .unwrap()
            .profiles
            .iter()
            .find(|profile| profile.profile_id == "profile-a")
            .unwrap()
            .content_revision,
        2
    );
    assert_eq!(
        restarted_jobs.get_job(OWNER, &job_id).unwrap().state,
        JobState::Succeeded
    );
}

#[test]
fn catalog_rejects_wrong_game_and_ambiguous_remote_keys_without_rebinding() {
    let catalog = DownloadCatalog::default();
    catalog.register_provider(Arc::new(FixtureProvider::direct(HashMap::new())), MODULE_DIGEST).unwrap();
    let binding = SourceGameBinding { game_id:"game".into(),remote_game_key:"remote".into(),provider_game_id:"signed-provider-game".into() };
    catalog.register_game("fixture",binding.clone()).unwrap();
    let conflict = SourceGameBinding { game_id:"different-game".into(),remote_game_key:"remote".into(),provider_game_id:"different-provider-game".into() };
    assert!(catalog.register_game("fixture",conflict).is_err());
    assert_eq!(catalog.lookup("fixture","game",Some("remote")).unwrap().1,binding);
    assert!(catalog.lookup("fixture","different-game",Some("remote")).is_err());
    assert!(catalog.lookup("fixture","game",Some("unsigned-key")).is_err());
    catalog.register_game("fixture",SourceGameBinding {game_id:"game".into(),remote_game_key:"second".into(),provider_game_id:"second-provider-game".into()}).unwrap();
    assert!(catalog.lookup("fixture","game",None).is_err());
    assert_eq!(catalog.lookup("fixture","game",Some("remote")).unwrap().1,binding);
}
