use std::{
    fs::{self, File, OpenOptions},
    future::Future,
    io::{self, Write},
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use fs2::FileExt;
use mintcat_runtime_kernel::RuntimePaths;
use mintcat_sdk::{AppError, JsonRpcRequest, JsonRpcResponse, ValidatedRequest};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions},
    task::JoinSet,
    time,
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::{Pipes::GetNamedPipeClientProcessId, Threading::GetCurrentProcessId},
};

use super::{
    read_json, serve_connection, write_json, ClientHello, IpcError, ServerHello, ServerReady,
    HANDSHAKE_TIMEOUT, IPC_PROTOCOL_VERSION, MAX_FRAME_BYTES,
};

pub struct IpcServer {
    listener: NamedPipeServer,
    pipe_name: String,
    generation_id: String,
    _instance_lock: InstanceLock,
}

pub struct IpcClient {
    stream: NamedPipeClient,
    pub generation_id: String,
}

struct InstanceLock {
    _file: File,
}

impl IpcServer {
    pub fn bind(paths: &RuntimePaths) -> Result<Self, IpcError> {
        let layout = paths.layout();
        fs::create_dir_all(&layout.runtime_dir)?;
        let instance_lock = InstanceLock::acquire(&layout.runtime_dir.join("daemon.lock"))?;
        let pipe_name = pipe_name(paths.data_dir());
        let listener = create_pipe(&pipe_name, true)?;
        Ok(Self {
            listener,
            pipe_name,
            generation_id: Uuid::new_v4().to_string(),
            _instance_lock: instance_lock,
        })
    }

    pub fn ready(&self) -> ServerReady {
        ServerReady {
            generation_id: self.generation_id.clone(),
            endpoint: PathBuf::from(&self.pipe_name),
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
        let mut listener = self.listener;
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                connected = listener.connect() => {
                    connected?;
                    let stream = listener;
                    listener = create_pipe(&self.pipe_name, false)?;
                    let peer_user_id = peer_process_id(&stream)?;
                    let handler = Arc::clone(&handler);
                    let active_requests = Arc::clone(&active_requests);
                    let generation_id = self.generation_id.clone();
                    connections.spawn(async move {
                        if let Err(error) = serve_connection(
                            stream,
                            &peer_user_id,
                            None,
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
        Ok(())
    }
}

impl IpcClient {
    pub async fn connect(paths: &RuntimePaths) -> Result<Self, IpcError> {
        let name = pipe_name(paths.data_dir());
        let mut stream = time::timeout(HANDSHAKE_TIMEOUT, async {
            loop {
                match ClientOptions::new().open(&name) {
                    Ok(stream) => return Ok(stream),
                    Err(error) if error.raw_os_error() == Some(231) => {
                        time::sleep(Duration::from_millis(20)).await;
                    }
                    Err(error) => return Err(error),
                }
            }
        })
        .await
        .map_err(|_| IpcError::Deadline)?
        .map_err(|error| IpcError::Unavailable(format!("{name}: {error}")))?;
        let hello = ClientHello {
            kind: "hello".to_owned(),
            protocol_version: IPC_PROTOCOL_VERSION,
            client_id: Uuid::new_v4().to_string(),
            claimed_user_id: current_process_id(),
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

impl InstanceLock {
    fn acquire(path: &Path) -> Result<Self, IpcError> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        FileExt::try_lock_exclusive(&file)
            .map_err(|_| IpcError::InstanceRunning(path.to_path_buf()))?;
        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        file.sync_all()?;
        Ok(Self { _file: file })
    }
}

fn create_pipe(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options.reject_remote_clients(true);
    if first {
        options.first_pipe_instance(true);
    }
    options.create(name)
}

fn pipe_name(data_dir: &Path) -> String {
    let digest = Sha256::digest(data_dir.to_string_lossy().as_bytes());
    let mut suffix = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write;
        write!(suffix, "{byte:02x}").expect("writing to a string cannot fail");
    }
    format!(r"\\.\pipe\mintcat-vnext-{suffix}")
}

fn current_process_id() -> String {
    // SAFETY: GetCurrentProcessId has no preconditions.
    let process_id = unsafe { GetCurrentProcessId() };
    format!("pid:{process_id}")
}

fn peer_process_id(stream: &NamedPipeServer) -> Result<String, IpcError> {
    let mut process_id = 0_u32;
    // SAFETY: the handle belongs to a connected named-pipe server and process_id is writable.
    let succeeded =
        unsafe { GetNamedPipeClientProcessId(stream.as_raw_handle() as HANDLE, &mut process_id) };
    if succeeded == 0 {
        return Err(IpcError::Io(io::Error::last_os_error()));
    }
    Ok(format!("pid:{process_id}"))
}
