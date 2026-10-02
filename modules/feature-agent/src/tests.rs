use super::*;
use std::{io::{Read, Write}, net::TcpListener, thread, time::{SystemTime, UNIX_EPOCH}};
use mintcat_runtime_broker::{CredentialVault, MemoryCredentialStore, SecretValue};
use mintcat_runtime_jobs::{ActionSpec, ConfirmationPolicy, EventUpdate, JobStore, RegisterJob};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{ActionState, JobState};
use tempfile::TempDir;

fn target() -> SessionTarget { SessionTarget { game_id: "game:drg".into(), profile_id: "profile-1".into(), profile_revision: 7, content_revision: 11, installation_id: "installation-1".into(), installation_revision: 3 } }
struct ReadGateway { jobs: JobStore }
#[async_trait]
impl ApplicationGateway for ReadGateway {
    async fn execute(&self, request: ValidatedRequest) -> Result<Value, AppError> {
        // A manually fabricated approval must never reach this gateway at all.
        assert_eq!(request.envelope.method, "actions.get");
        let id = request.envelope.params["actionId"].as_str().unwrap();
        Ok(serde_json::to_value(self.jobs.get_action("human", id).unwrap()).unwrap())
    }
}
struct UnusedGateway;
#[async_trait]
impl ApplicationGateway for UnusedGateway {
    async fn execute(&self, _: ValidatedRequest) -> Result<Value, AppError> { panic!("denied tools must not execute"); }
}
fn broker() -> (CredentialVault, NetworkBroker) {
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let network = NetworkBroker::new(&vault, None).unwrap(); (vault, network)
}

// A real TCP OpenAI-compatible HTTP provider. There is no mocked completion client.
fn provider(responses: Vec<(u16, Value)>, bearer: Option<String>) -> (String, thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let mut bodies = Vec::new();
        for (status, response) in responses {
            let (mut socket, _) = listener.accept().unwrap(); socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut received = Vec::new(); let mut buffer = [0; 4096];
            let (offset, length) = loop {
                let count = socket.read(&mut buffer).unwrap(); assert_ne!(count, 0); received.extend_from_slice(&buffer[..count]);
                if let Some(position) = received.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = String::from_utf8(received[..position].to_vec()).unwrap();
                    assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1"));
                    let length: usize = headers.lines().find_map(|line| { let (name, value) = line.split_once(':')?; name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap()) }).unwrap();
                    if let Some(token) = &bearer { assert!(headers.lines().any(|line| line.eq_ignore_ascii_case(&format!("authorization: Bearer {token}")))); }
                    break (position + 4, length);
                }
            };
            while received.len() < offset + length { let count = socket.read(&mut buffer).unwrap(); assert_ne!(count, 0); received.extend_from_slice(&buffer[..count]); }
            bodies.push(serde_json::from_slice(&received[offset..offset + length]).unwrap());
            let bytes = response.to_string();
            write!(socket, "HTTP/1.1 {status} Controlled\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{bytes}", bytes.len()).unwrap();
        }
        bodies
    });
    (format!("http://{address}/v1"), worker)
}
fn final_response() -> Value { json!({"choices":[{"message":{"role":"assistant","content":"Review the durable action in MintCat; only a human can approve it."}}]}) }

#[test]
fn manual_approval_and_wrong_target_are_rejected_before_execution() {
    for name in ["actions__approve", "actions.approve", "install__apply", "install__uninstall", "updates__activate"] {
        assert_eq!(decode_tool(name, "{}", &target(), "daemon-1", "key").unwrap_err(), AgentError::PermissionDenied);
    }
    let call = decode_tool("mods__list", r#"{"profileId":"other-profile"}"#, &target(), "daemon-1", "key").unwrap_err();
    assert_eq!(call, AgentError::TargetMismatch);
    let missing = decode_tool("mods__list", "{}", &target(), "daemon-1", "key").unwrap_err();
    assert_eq!(missing, AgentError::InvalidTool);
    let stale = decode_tool("mods__setEnabledBatch", r#"{"profileId":"profile-1","expectedContentRevision":10,"enabled":true,"profileMods":[]}"#, &target(), "daemon-1", "key").unwrap_err();
    assert_eq!(stale, AgentError::TargetMismatch);
    let plan = decode_tool("install__plan", r#"{"profileId":"profile-1","revision":8,"contentRevision":11,"gameId":"game:drg","installationId":"installation-1","options":{}}"#, &target(), "daemon-1", "key").unwrap_err();
    assert_eq!(plan, AgentError::TargetMismatch);
}

#[tokio::test]
async fn actual_model_suggestion_cannot_approve_and_session_disposal_retains_action_binding() {
    let sandbox = TempDir::new().unwrap();
    let layout = RuntimePaths::new(sandbox.path().join("runtime")).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
    let jobs = JobStore::open(KernelDatabase::open(&layout).unwrap(), "daemon-1", 100).unwrap();
    let job = jobs.register_job(RegisterJob { command: "install.plan".into(), owner_scope: "human".into(), idempotency_key: "plan-1".into(), request_hash: "plan-hash".into(), target: json!({"profileId":"profile-1","contentRevision":11}), confirmation_policy: ConfirmationPolicy::Required }).unwrap().job;
    jobs.append_event(&job.job_id, EventUpdate::state("preparing", JobState::Preparing)).unwrap();
    let action = jobs.create_action(&job.job_id, ActionSpec {
        kind: "install".into(), operation_id: job.job_id.clone(), target: json!({"profileId":"profile-1","contentRevision":11}), target_revision: 11,
        plan_hash: format!("sha256:{}", "1".repeat(64)), module_lock_digest: format!("sha256:{}", "2".repeat(64)),
        expires_at_ms: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64 + 60000,
        response_schema: json!({"type":"object","additionalProperties":false,"required":["approved"],"properties":{"approved":{"type":"boolean"}}}),
    }).unwrap();
    let suggestion = json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
        {"id":"approve","type":"function","function":{"name":"actions__approve","arguments":json!({"actionId":action.action_id,"expectedRevision":action.revision,"response":{"approved":true}}).to_string()}},
        {"id":"inspect","type":"function","function":{"name":"actions__get","arguments":json!({"actionId":action.action_id}).to_string()}},
        {"id":"forged","type":"function","function":{"name":"test-secret-token","arguments":"{}"}}
    ]}}]});
    let (url, server) = provider(vec![(200, suggestion), (200, final_response())], Some("test-secret-token".into()));
    let (vault, network) = broker();
    let credential = vault.store(SecretValue::new(b"test-secret-token".to_vec()).unwrap()).unwrap();
    let gateway = Arc::new(ReadGateway { jobs: jobs.clone() });
    let db = sandbox.path().join("conversations.sqlite3");
    let sessions = AgentSessions::open(&db, network.clone(), gateway.clone()).unwrap();
    let session = sessions.create(CreateSession { configuration: AgentConfiguration { base_url: url, model: "controlled".into(), credential_ref: Some(credential.as_str().into()) }, target: target() }).unwrap();
    let result = sessions.send(SendMessage { session_id: session.session_id.clone(), expected_revision: 1, content: "Suggest the requested installation".into() }, "daemon-1").await.unwrap();
    assert_eq!(result.state, "idle"); assert_eq!(result.revision, 3);
    assert!(result.timeline.iter().any(|item| item.content.contains("PERMISSION_DENIED")));
    assert_eq!(jobs.get_action("human", &action.action_id).unwrap().state, ActionState::Pending);
    assert!(result.bindings.iter().any(|binding| binding.action_id.as_deref() == Some(action.action_id.as_str())));
    drop(sessions);
    let reopened = AgentSessions::open(&db, network, gateway).unwrap();
    let persisted = reopened.get(&session.session_id).unwrap();
    assert_eq!(persisted.bindings, result.bindings);
    reopened.delete(&session.session_id, persisted.revision).unwrap();
    assert!(matches!(reopened.get(&session.session_id), Err(AgentError::NotFound)));
    assert_eq!(jobs.get_action("human", &action.action_id).unwrap().state, ActionState::Pending);
    let requests = server.join().unwrap();
    let last = requests.last().unwrap()["messages"].as_array().unwrap();
    assert!(last.iter().filter(|message| message["role"] == "tool").any(|message| message["content"].as_str().unwrap().contains("PERMISSION_DENIED")));
    let exported = serde_json::to_string(&result).unwrap(); assert!(!exported.contains("test-secret-token"));
}

#[tokio::test]
async fn provider_error_never_exposes_bearer_or_prompt_and_failed_turn_can_resume() {
    let sandbox = TempDir::new().unwrap();
    let (url, server) = provider(vec![(401, json!({"error":{"message":"secret-token private-user-prompt"}})), (200, final_response())], Some("secret-token".into()));
    let (vault, network) = broker();
    let credential = vault.store(SecretValue::new(b"secret-token".to_vec()).unwrap()).unwrap();
    let sessions = AgentSessions::open(sandbox.path().join("sessions.sqlite3"), network, Arc::new(UnusedGateway)).unwrap();
    let session = sessions.create(CreateSession { configuration: AgentConfiguration { base_url: url, model: "controlled".into(), credential_ref: Some(credential.as_str().into()) }, target: target() }).unwrap();
    let error = sessions.send(SendMessage { session_id: session.session_id.clone(), expected_revision: 1, content: "private-user-prompt".into() }, "daemon-1").await.err().unwrap();
    assert_eq!(error, AgentError::Provider);
    assert!(!error.to_string().contains("secret-token")); assert!(!error.to_string().contains("private-user-prompt"));
    let failed = sessions.get(&session.session_id).unwrap();
    assert_eq!(failed.state, "failed"); assert_eq!(failed.error_code.as_deref(), Some("AGENT_PROVIDER"));
    assert!(failed.timeline.iter().filter(|item| item.role == "error").all(|item| !item.content.contains("secret-token") && !item.content.contains("private-user-prompt")));
    assert!(matches!(sessions.delete(&session.session_id, 1), Err(AgentError::StaleRevision)));
    let resumed = sessions.send(SendMessage { session_id: session.session_id, expected_revision: failed.revision, content: "Continue".into() }, "daemon-1").await.unwrap();
    assert_eq!(resumed.state, "idle"); assert_eq!(resumed.revision, 5); assert!(resumed.error_code.is_none());
    server.join().unwrap();
}

#[tokio::test]
async fn abandoned_provider_turn_preserves_user_turn_and_rejects_concurrent_send_delete() {
    let sandbox = TempDir::new().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (_, network) = broker();
    let sessions = AgentSessions::open(sandbox.path().join("sessions.sqlite3"), network, Arc::new(UnusedGateway)).unwrap();
    let session = sessions.create(CreateSession { configuration: AgentConfiguration { base_url: format!("http://{}/v1", listener.local_addr().unwrap()), model: "controlled".into(), credential_ref: None }, target: target() }).unwrap();
    {
        let turn = sessions.send(SendMessage { session_id: session.session_id.clone(), expected_revision: 1, content: "Keep this turn".into() }, "daemon-1");
        tokio::pin!(turn);
        tokio::select! { result = &mut turn => panic!("unexpected response: {:?}", result.err()), _ = tokio::time::sleep(Duration::from_millis(50)) => {} }
        assert_eq!(sessions.get(&session.session_id).unwrap().state, "sending");
        assert!(matches!(sessions.delete(&session.session_id, 2), Err(AgentError::Busy)));
        assert!(matches!(sessions.send(SendMessage { session_id: session.session_id.clone(), expected_revision: 2, content: "Other".into() }, "daemon-1").await, Err(AgentError::Busy)));
    }
    let recovered = sessions.get(&session.session_id).unwrap();
    assert_eq!(recovered.state, "failed");
    assert_eq!(recovered.error_code.as_deref(), Some("TURN_INTERRUPTED"));
    assert!(recovered.timeline.iter().any(|item| item.role == "user" && item.content == "Keep this turn"));
}
