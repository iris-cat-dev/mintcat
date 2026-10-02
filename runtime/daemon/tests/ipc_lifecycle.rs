#[cfg(unix)]
mod unix {

    use std::{fs, path::Path};

    use mintcat_daemon::BoundDaemon;
    use mintcat_library_service::{CredentialVault, LegacyCredential};
    use mintcat_runtime_ipc::IpcClient;
    use mintcat_runtime_jobs::{
        request_hash, ActionSpec, ConfirmationPolicy, EventUpdate, RegisterJob,
    };
    use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
    use mintcat_sdk::{JobState, JsonRpcId, JsonRpcRequest, JsonRpcResponse};
    use serde_json::{json, Value};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::tempdir;
    use tokio::{
        sync::oneshot,
        task::JoinHandle,
    };

    struct RunningDaemon {
        paths: RuntimePaths,
        generation_id: String,
        shutdown: oneshot::Sender<()>,
        task: JoinHandle<Result<(), mintcat_daemon::DaemonError>>,
    }

    impl RunningDaemon {
        async fn start(data_dir: &Path) -> Self {
            let paths = RuntimePaths::new(data_dir).unwrap();
            let daemon = BoundDaemon::bind(&paths, RuntimeEnvironment::Test).unwrap();
            let generation_id = daemon.ready().generation_id;
            let (shutdown, receiver) = oneshot::channel();
            let task = tokio::spawn(daemon.run(async {
                let _ = receiver.await;
            }));
            Self {
                paths,
                generation_id,
                shutdown,
                task,
            }
        }

        async fn start_bound(paths: RuntimePaths, daemon: BoundDaemon) -> Self {
            let generation_id = daemon.ready().generation_id;
            let (shutdown, receiver) = oneshot::channel();
            let task = tokio::spawn(daemon.run(async {
                let _ = receiver.await;
            }));
            Self {
                paths,
                generation_id,
                shutdown,
                task,
            }
        }

        async fn stop(self) {
            self.shutdown.send(()).unwrap();
            self.task.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn two_clients_share_one_generation_and_reconnect_after_disconnect() {
        let sandbox = tempdir().unwrap();
        let daemon = RunningDaemon::start(&sandbox.path().join("data")).await;

        let mut first = IpcClient::connect(&daemon.paths).await.unwrap();
        let mut second = IpcClient::connect(&daemon.paths).await.unwrap();
        assert_eq!(first.generation_id, daemon.generation_id);
        assert_eq!(second.generation_id, daemon.generation_id);

        let first_response = request(&mut first, "system.status", "first-status").await;
        let second_response = request(&mut second, "system.status", "second-status").await;
        assert_eq!(
            first_response.id,
            JsonRpcId::String("first-status".to_owned())
        );
        assert_eq!(
            second_response.id,
            JsonRpcId::String("second-status".to_owned())
        );
        assert_eq!(
            first_response.result.as_ref().unwrap()["generationId"],
            daemon.generation_id
        );
        assert_eq!(
            second_response.result.as_ref().unwrap()["generationId"],
            daemon.generation_id
        );

        let stale = second
            .request(&JsonRpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: JsonRpcId::String("stale-session".to_owned()),
                method: "system.status".to_owned(),
                contract_version: 1,
                session_generation: "previous-generation".to_owned(),
                deadline_ms: 5_000,
                idempotency_key: None,
                params: json!({}),
            })
            .await
            .unwrap();
        assert_eq!(stale.error.unwrap().code, "STALE_SESSION");
        let after_stale = request(&mut second, "system.status", "after-stale").await;
        assert_eq!(
            after_stale.result.unwrap()["generationId"],
            daemon.generation_id
        );

        drop(first);
        let mut reconnected = IpcClient::connect(&daemon.paths).await.unwrap();
        let doctor = request(&mut reconnected, "system.doctor", "doctor-after-reconnect").await;
        assert_eq!(doctor.result.as_ref().unwrap()["healthy"], true);

        let endpoint = daemon.paths.layout().endpoint;
        daemon.stop().await;
        assert!(!endpoint.exists());
    }

    #[tokio::test]
    async fn durable_job_and_action_apis_survive_disconnect_and_restart() {
        let sandbox = tempdir().unwrap();
        let paths = RuntimePaths::new(sandbox.path().join("durable")).unwrap();
        let bound = BoundDaemon::bind(&paths, RuntimeEnvironment::Test).unwrap();
        let jobs = bound.job_store().clone();

        let registration = jobs
            .register_job(RegisterJob {
                command: "fixture.long-operation".to_owned(),
                owner_scope: "local-user".to_owned(),
                idempotency_key: "job-operation".to_owned(),
                request_hash: request_hash(&json!({ "input": 1 })).unwrap(),
                target: json!({ "profileId": "profile-1" }),
                confirmation_policy: ConfirmationPolicy::None,
            })
            .unwrap();
        jobs.append_event(
            &registration.job.job_id,
            EventUpdate::state("preparing", JobState::Preparing),
        )
        .and_then(|_| {
            jobs.append_event(
                &registration.job.job_id,
                EventUpdate::state("running", JobState::Running),
            )
        })
        .unwrap();

        let action_job = jobs
            .register_job(RegisterJob {
                command: "fixture.confirmed-operation".to_owned(),
                owner_scope: "local-user".to_owned(),
                idempotency_key: "action-operation".to_owned(),
                request_hash: request_hash(&json!({ "input": 2 })).unwrap(),
                target: json!({ "profileId": "profile-2", "revision": 3 }),
                confirmation_policy: ConfirmationPolicy::Required,
            })
            .unwrap();
        jobs.append_event(
            &action_job.job.job_id,
            EventUpdate::state("preparing", JobState::Preparing),
        )
        .unwrap();
        let action = jobs
            .create_action(
                &action_job.job.job_id,
                ActionSpec {
                    kind: "confirm-plan".to_owned(),
                    operation_id: "action-operation".to_owned(),
                    target: json!({ "profileId": "profile-2", "revision": 3 }),
                    target_revision: 3,
                    plan_hash: format!("sha256:{}", "3".repeat(64)),
                    module_lock_digest: format!("sha256:{}", "4".repeat(64)),
                    expires_at_ms: i64::try_from(
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_millis(),
                    )
                    .unwrap()
                        + 60_000,
                    response_schema: json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["approved"],
                        "properties": { "approved": { "type": "boolean" } }
                    }),
                },
            )
            .unwrap();

        let daemon = RunningDaemon::start_bound(paths, bound).await;
        let mut client = IpcClient::connect(&daemon.paths).await.unwrap();
        let snapshot = successful_result(
            request_with(
                &mut client,
                "jobs.get",
                "job-get",
                None,
                json!({ "jobId": registration.job.job_id }),
            )
            .await,
        );
        assert_eq!(snapshot["state"], "running");
        let page = successful_result(
            request_with(
                &mut client,
                "jobs.watch",
                "job-watch",
                None,
                json!({ "jobId": registration.job.job_id, "afterSeq": 0, "limit": 100 }),
            )
            .await,
        );
        assert_eq!(page["resetRequired"], false);
        assert_eq!(page["events"].as_array().unwrap().len(), 2);

        let cancelled = successful_result(
            request_with(
                &mut client,
                "jobs.cancel",
                "job-cancel",
                Some("cancel-operation"),
                json!({
                    "jobId": registration.job.job_id,
                    "expectedRevision": snapshot["lastSeq"],
                }),
            )
            .await,
        );
        assert_eq!(cancelled["state"], "cancelling");
        drop(client);

        let mut reconnected = IpcClient::connect(&daemon.paths).await.unwrap();
        let after_disconnect = successful_result(
            request_with(
                &mut reconnected,
                "jobs.get",
                "job-after-disconnect",
                None,
                json!({ "jobId": registration.job.job_id }),
            )
            .await,
        );
        assert_eq!(after_disconnect["state"], "cancelling");
        let pending_action = successful_result(
            request_with(
                &mut reconnected,
                "actions.get",
                "action-get",
                None,
                json!({ "actionId": action.action_id }),
            )
            .await,
        );
        assert_eq!(pending_action["state"], "pending");
        assert_eq!(pending_action["targetRevision"], 3);
        assert_eq!(
            pending_action["moduleLockDigest"],
            format!("sha256:{}", "4".repeat(64))
        );
        let approval = successful_result(
            request_with(
                &mut reconnected,
                "actions.approve",
                "action-approve",
                Some("approval-operation"),
                json!({
                    "actionId": action.action_id,
                    "expectedRevision": action.revision,
                    "response": { "approved": true },
                }),
            )
            .await,
        );
        assert_eq!(approval["jobId"], action_job.job.job_id);
        drop(reconnected);
        daemon.stop().await;

        let restarted = RunningDaemon::start(&sandbox.path().join("durable")).await;
        let mut restarted_client = IpcClient::connect(&restarted.paths).await.unwrap();
        let recovered = successful_result(
            request_with(
                &mut restarted_client,
                "jobs.get",
                "job-after-restart",
                None,
                json!({ "jobId": registration.job.job_id }),
            )
            .await,
        );
        assert_eq!(recovered["state"], "recovery-required");
        let durable_action = successful_result(
            request_with(
                &mut restarted_client,
                "actions.get",
                "action-after-restart",
                None,
                json!({ "actionId": action.action_id }),
            )
            .await,
        );
        assert_eq!(durable_action["state"], "approved");
        drop(restarted_client);
        restarted.stop().await;
    }

    struct EmptyVault;

    impl CredentialVault for EmptyVault {
        fn store(&self, _credential: LegacyCredential<'_>) -> Result<String, String> {
            Err("fixture unexpectedly contained a credential".to_owned())
        }

        fn delete(&self, _credential_ref: &str) -> Result<(), String> {
            Ok(())
        }
    }


    #[tokio::test]
    async fn library_queries_edits_and_unprovisioned_source_rejection_share_typed_ipc() {
        let sandbox = tempdir().unwrap();
        let paths = RuntimePaths::new(sandbox.path().join("library")).unwrap();
        let bound = BoundDaemon::bind(&paths, RuntimeEnvironment::Test).unwrap();
        let legacy = sandbox.path().join("legacy.sqlite");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/baseline/legacy/mintcat-v0.6.sqlite"),
            &legacy,
        )
        .unwrap();
        bound
            .library_store()
            .import_legacy(&legacy, &EmptyVault)
            .unwrap();
        let daemon = RunningDaemon::start_bound(paths, bound).await;
        let mut client = IpcClient::connect(&daemon.paths).await.unwrap();

        let games = successful_result(
            request_with(
                &mut client,
                "games.list",
                "games",
                None,
                json!({"pageSize": 10}),
            )
            .await,
        );
        assert_eq!(games["items"].as_array().unwrap().len(), 2);
        let profiles = successful_result(
            request_with(
                &mut client,
                "profiles.list",
                "profiles",
                None,
                json!({"gameId": "game:drg", "pageSize": 10}),
            )
            .await,
        );
        let profile = &profiles["items"][0];
        let profile_id = profile["profileId"].as_str().unwrap();
        let groups = successful_result(
            request_with(
                &mut client,
                "groups.list",
                "groups",
                None,
                json!({"profileId": profile_id}),
            )
            .await,
        );
        assert_eq!(groups["items"].as_array().unwrap().len(), 2);
        let mods = successful_result(
            request_with(
                &mut client,
                "mods.list",
                "mods",
                None,
                json!({"profileId": profile_id, "pageSize": 10}),
            )
            .await,
        );
        assert_eq!(mods["total"], 2);
        let first_mod = &mods["items"][0];
        let enabled = successful_result(
            request_with(
                &mut client,
                "mods.setEnabled",
                "disable",
                Some("ipc-disable"),
                json!({
                    "profileId": profile_id,
                    "profileModId": first_mod["profileModId"],
                    "expectedContentRevision": mods["contentRevision"],
                    "enabled": !first_mod["enabled"].as_bool().unwrap(),
                }),
            )
            .await,
        );
        let replay = successful_result(
            request_with(
                &mut client,
                "mods.setEnabled",
                "disable-replay",
                Some("ipc-disable"),
                json!({
                    "profileId": profile_id,
                    "profileModId": first_mod["profileModId"],
                    "expectedContentRevision": mods["contentRevision"],
                    "enabled": !first_mod["enabled"].as_bool().unwrap(),
                }),
            )
            .await,
        );
        assert_eq!(replay, enabled);
        let conflict = request_with(
            &mut client,
            "mods.setEnabled",
            "disable-conflict",
            Some("ipc-disable"),
            json!({
                "profileId": profile_id,
                "profileModId": first_mod["profileModId"],
                "expectedContentRevision": mods["contentRevision"],
                "enabled": first_mod["enabled"],
            }),
        )
        .await;
        assert_eq!(conflict.error.unwrap().code, "CONFLICT");

        let details = successful_result(
            request_with(
                &mut client,
                "mods.get",
                "mod-details",
                None,
                json!({
                    "profileId": profile_id,
                    "profileModId": first_mod["profileModId"],
                }),
            )
            .await,
        );
        let version_id = details["versions"][0]["versionId"].clone();
        let refused = request_with(
            &mut client,
            "mods.setVersion",
            "version-download",
            Some("ipc-version"),
            json!({
                "profileId": profile_id,
                "profileModId": first_mod["profileModId"],
                "incarnationId": first_mod["incarnationId"],
                "expectedContentRevision": enabled["contentRevision"],
                "selection": {"kind": "pinned", "versionId": version_id},
                "mode": "prepare-and-apply",
            }),
        ).await;
        assert_eq!(refused.error.unwrap().code, "MODULE_UNAVAILABLE");
        let content_state = successful_result(
            request_with(
                &mut client,
                "profiles.contentState",
                "content-state",
                None,
                json!({"profileId": profile_id}),
            )
            .await,
        );
        assert_eq!(content_state["contentRevision"], enabled["contentRevision"]);
        assert_eq!(content_state["writable"], true);
        assert_eq!(content_state["recoveryRequired"], false);
        assert!(content_state["busyOperationId"].is_null());
        assert!(content_state["operations"].as_array().unwrap().is_empty());

        drop(client);
        daemon.stop().await;
    }

    #[tokio::test]
    async fn independent_data_directories_have_independent_daemons() {
        let sandbox = tempdir().unwrap();
        let first = RunningDaemon::start(&sandbox.path().join("first")).await;
        let second = RunningDaemon::start(&sandbox.path().join("second")).await;
        assert_ne!(first.generation_id, second.generation_id);

        let mut first_client = IpcClient::connect(&first.paths).await.unwrap();
        let mut second_client = IpcClient::connect(&second.paths).await.unwrap();
        let first_status =
            successful_result(request(&mut first_client, "system.status", "first").await);
        let second_status =
            successful_result(request(&mut second_client, "system.status", "second").await);

        assert_eq!(
            first_status["dataDir"],
            first.paths.data_dir().to_string_lossy().as_ref()
        );
        assert_eq!(
            second_status["dataDir"],
            second.paths.data_dir().to_string_lossy().as_ref()
        );
        assert_ne!(first_status["dataDir"], second_status["dataDir"]);

        drop(first_client);
        drop(second_client);
        first.stop().await;
        second.stop().await;
    }

    async fn request(client: &mut IpcClient, method: &str, id: &str) -> JsonRpcResponse {
        request_with(client, method, id, None, json!({})).await
    }

    async fn request_with(
        client: &mut IpcClient,
        method: &str,
        id: &str,
        idempotency_key: Option<&str>,
        params: Value,
    ) -> JsonRpcResponse {
        client
            .request(&JsonRpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: JsonRpcId::String(id.to_owned()),
                method: method.to_owned(),
                contract_version: 1,
                session_generation: client.generation_id.clone(),
                deadline_ms: 5_000,
                idempotency_key: idempotency_key.map(str::to_owned),
                params,
            })
            .await
            .unwrap()
    }

    fn successful_result(response: JsonRpcResponse) -> Value {
        assert!(response.error.is_none(), "{:?}", response.error);
        response.result.unwrap()
    }
}
