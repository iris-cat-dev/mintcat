use std::{
    fs::OpenOptions,
    future::Future,
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
#[cfg(unix)]
use mintcat_sdk::JsonRpcRequest;
use mintcat_sdk::{
    decode_request_value, validate_command_result, AppError, JsonRpcId, JsonRpcResponse,
    RecoveryAction, ValidatedRequest,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
#[cfg(unix)]
use tokio::task::JoinSet;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time,
};
#[cfg(unix)]
use uuid::Uuid;

#[cfg(unix)]
use fs2::FileExt;
#[cfg(unix)]
use std::{
    fs::{self, File},
    os::unix::fs::{FileTypeExt, PermissionsExt},
};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

pub const IPC_PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
pub const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);
pub const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("another daemon owns the data directory lock at {0}")]
    InstanceRunning(PathBuf),
    #[error("local IPC endpoint is unavailable: {0}")]
    Unavailable(String),
    #[error("local IPC I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("peer identity {actual} does not match expected identity {expected}")]
    PeerIdentity { expected: String, actual: String },
    #[error("IPC protocol {actual} is incompatible with {IPC_PROTOCOL_VERSION}")]
    ProtocolVersion { actual: u32 },
    #[error("IPC frame is {actual} bytes; maximum is {MAX_FRAME_BYTES}")]
    FrameTooLarge { actual: usize },
    #[error("IPC peer disconnected")]
    Disconnected,
    #[error("IPC operation exceeded its deadline")]
    Deadline,
    #[error("invalid IPC payload: {0}")]
    InvalidPayload(String),
    #[error("local IPC is unsupported on this platform")]
    UnsupportedPlatform,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientHello {
    pub kind: String,
    pub protocol_version: u32,
    pub client_id: String,
    pub claimed_user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerHello {
    pub kind: String,
    pub protocol_version: u32,
    pub generation_id: String,
    pub max_frame_bytes: usize,
    pub accepted: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServerReady {
    pub generation_id: String,
    pub endpoint: PathBuf,
    pub protocol_version: u32,
    pub max_frame_bytes: usize,
}

#[cfg(unix)]
pub struct IpcServer {
    listener: UnixListener,
    endpoint: PathBuf,
    generation_id: String,
    expected_user_id: String,
    _instance_lock: InstanceLock,
}

#[cfg(unix)]
pub struct IpcClient {
    stream: UnixStream,
    pub generation_id: String,
}

#[cfg(unix)]
struct InstanceLock {
    _file: File,
}

struct ActiveRequest {
    count: Arc<AtomicUsize>,
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(unix)]
impl IpcServer {
    pub fn bind(paths: &RuntimePaths) -> Result<Self, IpcError> {
        let layout = paths.layout();
        fs::create_dir_all(&layout.runtime_dir)?;
        fs::set_permissions(&layout.runtime_dir, fs::Permissions::from_mode(0o700))?;

        let lock_path = layout.runtime_dir.join("daemon.lock");
        let instance_lock = InstanceLock::acquire(&lock_path)?;
        if layout.endpoint.exists() {
            let endpoint_type = fs::symlink_metadata(&layout.endpoint)?.file_type();
            if !endpoint_type.is_socket() {
                fs::remove_file(&layout.endpoint)?;
            } else {
                match std::os::unix::net::UnixStream::connect(&layout.endpoint) {
                    Ok(_) => return Err(IpcError::InstanceRunning(lock_path)),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                        ) =>
                    {
                        fs::remove_file(&layout.endpoint)?;
                    }
                    Err(error) => return Err(IpcError::Io(error)),
                }
            }
        }
        let listener = UnixListener::bind(&layout.endpoint)?;
        fs::set_permissions(&layout.endpoint, fs::Permissions::from_mode(0o600))?;

        Ok(Self {
            listener,
            endpoint: layout.endpoint,
            generation_id: Uuid::new_v4().to_string(),
            expected_user_id: current_user_id(),
            _instance_lock: instance_lock,
        })
    }

    pub fn ready(&self) -> ServerReady {
        ServerReady {
            generation_id: self.generation_id.clone(),
            endpoint: self.endpoint.clone(),
            protocol_version: IPC_PROTOCOL_VERSION,
            max_frame_bytes: MAX_FRAME_BYTES,
        }
    }

    pub async fn run<F, Fut, Shutdown>(
        self,
        handler: Arc<F>,
        shutdown: Shutdown,
    ) -> Result<(), IpcError>
    where
        F: Fn(ValidatedRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, Box<AppError>>> + Send + 'static,
        Shutdown: Future<Output = ()> + Send,
    {
        let active_requests = Arc::new(AtomicUsize::new(0));
        let mut connections = JoinSet::new();
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted?;
                    let handler = Arc::clone(&handler);
                    let active_requests = Arc::clone(&active_requests);
                    let expected_user_id = self.expected_user_id.clone();
                    let generation_id = self.generation_id.clone();
                    connections.spawn(async move {
                        let peer_user_id = match peer_user_id(&stream) {
                            Ok(identity) => identity,
                            Err(error) => {
                                eprintln!("mintcatd peer authentication: {error}");
                                return;
                            }
                        };
                        if let Err(error) = serve_connection(
                            stream,
                            &peer_user_id,
                            Some(&expected_user_id),
                            &generation_id,
                            handler,
                            active_requests,
                        ).await {
                            eprintln!("mintcatd connection: {error}");
                        }
                    });
                }
            }
        }

        while active_requests.load(Ordering::Acquire) != 0 {
            time::sleep(Duration::from_millis(10)).await;
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        match fs::remove_file(&self.endpoint) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(IpcError::Io(error)),
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.endpoint);
    }
}

#[cfg(unix)]
impl InstanceLock {
    fn acquire(path: &Path) -> Result<Self, IpcError> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        FileExt::try_lock_exclusive(&file)
            .map_err(|_| IpcError::InstanceRunning(path.to_path_buf()))?;
        file.set_len(0)?;
        use std::io::Write;
        writeln!(file, "{}", std::process::id())?;
        file.sync_all()?;
        Ok(Self { _file: file })
    }
}

#[cfg(unix)]
impl IpcClient {
    pub async fn connect(paths: &RuntimePaths) -> Result<Self, IpcError> {
        let endpoint = paths.layout().endpoint;
        let mut stream = time::timeout(HANDSHAKE_TIMEOUT, UnixStream::connect(&endpoint))
            .await
            .map_err(|_| IpcError::Deadline)?
            .map_err(|error| IpcError::Unavailable(format!("{}: {error}", endpoint.display())))?;
        let hello = ClientHello {
            kind: "hello".to_owned(),
            protocol_version: IPC_PROTOCOL_VERSION,
            client_id: Uuid::new_v4().to_string(),
            claimed_user_id: current_user_id(),
        };
        write_json(&mut stream, &hello, HANDSHAKE_TIMEOUT).await?;
        let reply: ServerHello = read_json(&mut stream, HANDSHAKE_TIMEOUT).await?;
        if reply.protocol_version != IPC_PROTOCOL_VERSION {
            return Err(IpcError::ProtocolVersion {
                actual: reply.protocol_version,
            });
        }
        if !reply.accepted {
            return Err(IpcError::InvalidPayload(
                reply
                    .error
                    .unwrap_or_else(|| "handshake rejected".to_owned()),
            ));
        }
        Ok(Self {
            stream,
            generation_id: reply.generation_id,
        })
    }

    pub async fn request(&mut self, request: &JsonRpcRequest) -> Result<JsonRpcResponse, IpcError> {
        let deadline = Duration::from_millis(request.deadline_ms);
        write_json(&mut self.stream, request, deadline).await?;
        read_json(&mut self.stream, deadline).await
    }
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{IpcClient, IpcServer};

#[cfg(not(any(unix, windows)))]
pub struct IpcServer;

#[cfg(not(any(unix, windows)))]
pub struct IpcClient;

#[cfg(not(any(unix, windows)))]
impl IpcServer {
    pub fn bind(_paths: &RuntimePaths) -> Result<Self, IpcError> {
        Err(IpcError::UnsupportedPlatform)
    }
}

pub async fn connect_or_start(
    paths: &RuntimePaths,
    daemon_bin: &Path,
    environment: RuntimeEnvironment,
) -> Result<IpcClient, IpcError> {
    match IpcClient::connect(paths).await {
        Ok(client) => return Ok(client),
        Err(IpcError::Unavailable(_)) => {}
        Err(error) => return Err(error),
    }

    let layout = paths
        .initialize(environment)
        .map_err(|error| IpcError::Unavailable(error.to_string()))?;
    let daemon_log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(layout.runtime_dir.join("daemon.log"))?;
    let mut command = Command::new(daemon_bin);
    command
        .arg("--data-dir")
        .arg(paths.data_dir())
        .arg("--environment")
        .arg(environment_name(environment))
        .arg("--json")
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(daemon_log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    let mut child = command.spawn()?;

    let deadline = Instant::now() + DAEMON_START_TIMEOUT;
    loop {
        match IpcClient::connect(paths).await {
            Ok(client) => return Ok(client),
            Err(IpcError::Unavailable(_)) if Instant::now() < deadline => {
                if let Some(status) = child.try_wait()? {
                    return Err(IpcError::Unavailable(format!(
                        "daemon exited before readiness with {status}"
                    )));
                }
                time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn environment_name(environment: RuntimeEnvironment) -> &'static str {
    match environment {
        RuntimeEnvironment::Development => "development",
        RuntimeEnvironment::Test => "test",
        RuntimeEnvironment::Production => "production",
    }
}
#[cfg(not(any(unix, windows)))]
impl IpcClient {
    pub async fn connect(_paths: &RuntimePaths) -> Result<Self, IpcError> {
        Err(IpcError::UnsupportedPlatform)
    }
}

async fn serve_connection<S, F, Fut>(
    mut stream: S,
    peer_user_id: &str,
    expected_user_id: Option<&str>,
    generation_id: &str,
    handler: Arc<F>,
    active_requests: Arc<AtomicUsize>,
) -> Result<(), IpcError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: Fn(ValidatedRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, Box<AppError>>> + Send + 'static,
{
    let hello: ClientHello = read_json(&mut stream, HANDSHAKE_TIMEOUT).await?;
    let rejection = if hello.kind != "hello" {
        Some("first frame must be a hello".to_owned())
    } else if hello.protocol_version != IPC_PROTOCOL_VERSION {
        Some(format!(
            "protocol {} is incompatible with {}",
            hello.protocol_version, IPC_PROTOCOL_VERSION
        ))
    } else if hello.claimed_user_id != peer_user_id
        || expected_user_id.is_some_and(|expected| peer_user_id != expected)
    {
        Some(format!(
            "claimed identity {} does not match authenticated peer {}",
            hello.claimed_user_id, peer_user_id
        ))
    } else {
        None
    };
    let accepted = rejection.is_none();
    write_json(
        &mut stream,
        &ServerHello {
            kind: "hello-ack".to_owned(),
            protocol_version: IPC_PROTOCOL_VERSION,
            generation_id: generation_id.to_owned(),
            max_frame_bytes: MAX_FRAME_BYTES,
            accepted,
            error: rejection,
        },
        HANDSHAKE_TIMEOUT,
    )
    .await?;
    if !accepted {
        return Ok(());
    }

    loop {
        let value: Value = match read_json(&mut stream, IDLE_CONNECTION_TIMEOUT).await {
            Ok(value) => value,
            Err(IpcError::Disconnected) => return Ok(()),
            Err(error) => return Err(error),
        };
        let request_id = request_id(&value);
        let request = match decode_request_value(value) {
            Ok(request) => request,
            Err(error) => {
                let response = error_response(
                    request_id,
                    error.code,
                    &error.message,
                    json!({"path": error.path}),
                );
                write_json(&mut stream, &response, HANDSHAKE_TIMEOUT).await?;
                continue;
            }
        };
        if request.envelope.session_generation != generation_id {
            let response = error_response(
                request.envelope.id.clone(),
                "STALE_SESSION",
                "request session generation does not match the connected daemon",
                json!({
                    "expectedGeneration": generation_id,
                    "actualGeneration": request.envelope.session_generation
                }),
            );
            write_json(&mut stream, &response, HANDSHAKE_TIMEOUT).await?;
            continue;
        }
        let id = request.envelope.id.clone();
        let method = request.descriptor.method;
        let contract_version = request.descriptor.contract_version;
        let deadline = Duration::from_millis(request.envelope.deadline_ms);
        active_requests.fetch_add(1, Ordering::AcqRel);
        let _active = ActiveRequest {
            count: Arc::clone(&active_requests),
        };
        let response = match time::timeout(deadline, handler(request)).await {
            Ok(Ok(result)) => match validate_command_result(method, contract_version, &result) {
                Ok(()) => JsonRpcResponse {
                    jsonrpc: "2.0".to_owned(),
                    id,
                    result: Some(result),
                    error: None,
                },
                Err(error) => {
                    error_response(id, error.code, &error.message, json!({"path": error.path}))
                }
            },
            Ok(Err(error)) => JsonRpcResponse {
                jsonrpc: "2.0".to_owned(),
                id,
                result: None,
                error: Some(*error),
            },
            Err(_) => error_response(
                id,
                "DEADLINE_EXCEEDED",
                "request deadline exceeded",
                json!({}),
            ),
        };
        write_json(&mut stream, &response, deadline).await?;
    }
}

fn error_response(id: JsonRpcId, code: &str, message: &str, details: Value) -> JsonRpcResponse {
    let request_id = match &id {
        JsonRpcId::String(value) => value.clone(),
        JsonRpcId::Integer(value) => value.to_string(),
    };
    JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        id,
        result: None,
        error: Some(AppError {
            code: code.to_owned(),
            message_key: message.to_owned(),
            details,
            request_id,
            job_id: None,
            retryable: false,
            recovery_actions: Vec::<RecoveryAction>::new(),
        }),
    }
}

fn request_id(value: &Value) -> JsonRpcId {
    match value.get("id") {
        Some(Value::String(value)) if !value.is_empty() => JsonRpcId::String(value.clone()),
        Some(Value::Number(value)) => value
            .as_u64()
            .map(JsonRpcId::Integer)
            .unwrap_or_else(|| JsonRpcId::String("invalid-request".to_owned())),
        _ => JsonRpcId::String("invalid-request".to_owned()),
    }
}

async fn read_json<R, T>(reader: &mut R, deadline: Duration) -> Result<T, IpcError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let bytes = read_frame(reader, deadline).await?;
    serde_json::from_slice(&bytes).map_err(|error| IpcError::InvalidPayload(error.to_string()))
}

async fn write_json<W, T>(writer: &mut W, value: &T, deadline: Duration) -> Result<(), IpcError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes =
        serde_json::to_vec(value).map_err(|error| IpcError::InvalidPayload(error.to_string()))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            actual: bytes.len(),
        });
    }
    let length = u32::try_from(bytes.len()).expect("maximum frame fits u32");
    time::timeout(deadline, async {
        writer.write_all(&length.to_be_bytes()).await?;
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| IpcError::Deadline)??;
    Ok(())
}

async fn read_frame<R>(reader: &mut R, deadline: Duration) -> Result<Vec<u8>, IpcError>
where
    R: AsyncRead + Unpin,
{
    time::timeout(deadline, async {
        let mut prefix = [0_u8; 4];
        let read = reader.read(&mut prefix[..1]).await?;
        if read == 0 {
            return Err(IpcError::Disconnected);
        }
        reader.read_exact(&mut prefix[1..]).await?;
        let length = u32::from_be_bytes(prefix) as usize;
        if length > MAX_FRAME_BYTES {
            return Err(IpcError::FrameTooLarge { actual: length });
        }
        let mut bytes = vec![0_u8; length];
        reader.read_exact(&mut bytes).await?;
        Ok(bytes)
    })
    .await
    .map_err(|_| IpcError::Deadline)?
}

#[cfg(unix)]
fn current_user_id() -> String {
    // SAFETY: geteuid has no preconditions and does not dereference memory.
    let uid = unsafe { libc::geteuid() };
    format!("uid:{uid}")
}

#[cfg(unix)]
fn peer_user_id(stream: &UnixStream) -> Result<String, IpcError> {
    let credentials = stream.peer_cred()?;
    Ok(format!("uid:{}", credentials.uid()))
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, time::Duration};

    #[cfg(unix)]
    use std::sync::Arc;

    #[cfg(unix)]
    use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
    #[cfg(unix)]
    use mintcat_sdk::{AppError, JsonRpcId, JsonRpcRequest};
    #[cfg(unix)]
    use serde_json::{json, Value};
    #[cfg(unix)]
    use tempfile::tempdir;
    #[cfg(unix)]
    use tokio::{net::UnixStream, sync::oneshot};

    use super::{read_frame, write_json, IpcError, MAX_FRAME_BYTES};
    #[cfg(unix)]
    use super::{
        ClientHello, IpcClient, IpcServer, ServerHello, HANDSHAKE_TIMEOUT, IPC_PROTOCOL_VERSION,
    };

    #[tokio::test]
    async fn rejects_oversized_outgoing_frames() {
        let mut output = Cursor::new(Vec::new());
        let value = "x".repeat(MAX_FRAME_BYTES);
        let error = write_json(&mut output, &value, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(matches!(error, IpcError::FrameTooLarge { .. }));
    }

    #[tokio::test]
    async fn rejects_oversized_incoming_prefix_before_allocating_payload() {
        let length = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes();
        let mut input = Cursor::new(length);
        let error = read_frame(&mut input, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(matches!(error, IpcError::FrameTooLarge { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_a_forged_claimed_identity() {
        let sandbox = tempdir().unwrap();
        let paths = RuntimePaths::new(sandbox.path().join("data")).unwrap();
        paths.initialize(RuntimeEnvironment::Test).unwrap();
        let server = IpcServer::bind(&paths).unwrap();
        let endpoint = server.ready().endpoint;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(server.run(
            Arc::new(|_| async { Ok::<Value, Box<AppError>>(json!({})) }),
            async {
                let _ = shutdown_rx.await;
            },
        ));

        let mut stream = UnixStream::connect(endpoint).await.unwrap();
        write_json(
            &mut stream,
            &ClientHello {
                kind: "hello".to_owned(),
                protocol_version: IPC_PROTOCOL_VERSION,
                client_id: "forged-client".to_owned(),
                claimed_user_id: "uid:4294967294".to_owned(),
            },
            HANDSHAKE_TIMEOUT,
        )
        .await
        .unwrap();
        let reply: ServerHello = super::read_json(&mut stream, HANDSHAKE_TIMEOUT)
            .await
            .unwrap();
        assert!(!reply.accepted);
        assert!(reply.error.unwrap().contains("authenticated peer"));

        shutdown_tx.send(()).unwrap();
        task.await.unwrap().unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn enforces_the_client_request_deadline() {
        let sandbox = tempdir().unwrap();
        let paths = RuntimePaths::new(sandbox.path().join("data")).unwrap();
        paths.initialize(RuntimeEnvironment::Test).unwrap();
        let server = IpcServer::bind(&paths).unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(server.run(
            Arc::new(|_| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok::<Value, Box<AppError>>(json!({
                    "daemonVersion": "0.1.0",
                    "hostApi": "1",
                    "generationId": "late",
                    "dataDir": "/late",
                    "ready": true
                }))
            }),
            async {
                let _ = shutdown_rx.await;
            },
        ));
        let mut client = IpcClient::connect(&paths).await.unwrap();

        let error = client
            .request(&JsonRpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: JsonRpcId::String("deadline".to_owned()),
                method: "system.status".to_owned(),
                contract_version: 1,
                session_generation: client.generation_id.clone(),
                deadline_ms: 5,
                idempotency_key: None,
                params: json!({}),
            })
            .await
            .unwrap_err();

        assert!(matches!(error, IpcError::Deadline));
        drop(client);
        shutdown_tx.send(()).unwrap();
        task.await.unwrap().unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lock_rejects_a_second_daemon_for_the_same_data_directory() {
        let sandbox = tempdir().unwrap();
        let paths = RuntimePaths::new(sandbox.path().join("data")).unwrap();
        paths.initialize(RuntimeEnvironment::Test).unwrap();
        let first = IpcServer::bind(&paths).unwrap();
        let error = match IpcServer::bind(&paths) {
            Ok(_) => panic!("second daemon unexpectedly acquired the same lock"),
            Err(error) => error,
        };
        assert!(matches!(error, IpcError::InstanceRunning(_)));
        drop(first);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn removes_a_stale_socket_only_after_taking_the_instance_lock() {
        let sandbox = tempdir().unwrap();
        let paths = RuntimePaths::new(sandbox.path().join("data")).unwrap();
        let layout = paths.initialize(RuntimeEnvironment::Test).unwrap();
        std::fs::write(&layout.endpoint, b"stale-endpoint").unwrap();

        let server = IpcServer::bind(&paths).unwrap();

        assert!(layout.endpoint.exists());
        drop(server);
        assert!(!layout.endpoint.exists());
    }
}
