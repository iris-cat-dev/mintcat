use std::{path::PathBuf, sync::{Arc, RwLock}, time::Duration};
use mintcat_runtime_supervisor::{Invocation, ModuleSupervisor};
use tokio::{net::{TcpListener, TcpStream}, io::{AsyncReadExt, AsyncWriteExt}};
use serde_json::{json, Value};
#[derive(Clone)]
pub struct Generation { pub id: String, pub supervisor: ModuleSupervisor, pub payload: PathBuf, pub manifest: Arc<mintcat_sdk::ModuleManifest>, pub scope: String, pub contract: String, pub version: u64 }
pub type Surface = Arc<RwLock<Option<Generation>>>;
const HTML: &str = r#"<!doctype html><meta charset=utf-8><title>MintCat isolated module development</title><main id=app></main><pre id=status></pre><script type=module src="/host/index.js"></script>"#;
async fn response(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) -> std::io::Result<()> {
    let headers = format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\n\r\n", body.len());
    stream.write_all(headers.as_bytes()).await?; stream.write_all(body).await?; stream.shutdown().await
}
async fn serve(mut stream: TcpStream, current: Surface, origin: String, host_files: Arc<PathBuf>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut bytes = Vec::new(); let mut buffer = [0_u8; 4096];
    let header_end = loop {
        if bytes.len() > 16384 { return Err("HTTP headers too large".into()); }
        let n = stream.read(&mut buffer).await?; if n==0 { return Ok(()); } bytes.extend_from_slice(&buffer[..n]);
        if let Some(index) = bytes.windows(4).position(|w| w==b"\r\n\r\n") { break index+4; }
    };
    let header = std::str::from_utf8(&bytes[..header_end])?;
    let mut lines = header.split("\r\n"); let request = lines.next().ok_or("request missing")?;
    let fields = request.split_whitespace().collect::<Vec<_>>();
    if fields.len()!=3 { return Err("malformed request".into()); }
    let method = fields[0].to_owned(); let path = fields[1].to_owned();
    let mut length = 0; let mut request_origin = None; let mut host = None;
    for line in lines { if let Some((name,value)) = line.split_once(':') { match name.to_ascii_lowercase().as_str() { "content-length" => length = value.trim().parse::<usize>()?, "origin"=>request_origin=Some(value.trim().to_owned()), "host"=>host=Some(value.trim().to_owned()), _=>{} } } }
    if host.as_deref()!=Some(origin.trim_start_matches("http://")) || length > 1024*1024 { response(&mut stream,"403 Forbidden","application/json",b"{\"error\":\"local host boundary rejected\"}").await?; return Ok(()); }
    while bytes.len()-header_end < length { let n=stream.read(&mut buffer).await?; if n==0 { return Err("truncated body".into()); } bytes.extend_from_slice(&buffer[..n]); }
    let generation = current.read().map_err(|_| "generation lock poisoned")?.clone();
    let Some(generation) = generation else { response(&mut stream,"503 Service Unavailable","application/json",b"{\"error\":\"no healthy generation\"}").await?; return Ok(()); };
    if method=="GET" && path=="/" { response(&mut stream,"200 OK","text/html; charset=utf-8",HTML.as_bytes()).await?; }
    else if method=="GET" && path=="/generation" { response(&mut stream,"200 OK","application/json",&serde_json::to_vec(&json!({"id":generation.id,"contract":generation.contract,"version":generation.version}))?).await?; }
    else if method=="GET" && path=="/manifest" { response(&mut stream,"200 OK","application/json",&serde_json::to_vec(generation.manifest.as_ref())?).await?; }
    else if method=="GET" && path.starts_with("/host/") {
        let relative = &path[6..];
        if relative.is_empty() || relative.contains('%') || relative.contains('\\') || relative.split('/').any(|part| part.is_empty() || part=="." || part=="..") { response(&mut stream,"403 Forbidden","text/plain",b"invalid host path").await?; return Ok(()); }
        let file = host_files.join(relative); let metadata = std::fs::symlink_metadata(&file)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() { return Err("invalid host resource".into()); }
        let data = tokio::fs::read(file).await?;
        response(&mut stream,"200 OK",if relative.ends_with(".js") { "text/javascript" } else { "application/json" },&data).await?;
    }
    else if method=="GET" && path.starts_with("/payload/") {
        let relative=&path[9..];
        if relative.is_empty() || relative.contains('%') || relative.contains('\\') || relative.split('/').any(|part| part.is_empty() || part=="." || part=="..") { response(&mut stream,"403 Forbidden","text/plain",b"invalid payload path").await?; return Ok(()); }
        // Only verified immutable ArtifactStore paths are served; never the mutable development source tree.
        let payload = generation.payload.join(relative); let metadata=std::fs::symlink_metadata(&payload)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() { return Err("invalid payload file".into()); }
        let data=tokio::fs::read(&payload).await?;
        let content_type=if relative.ends_with(".js") || relative.ends_with(".mjs") { "text/javascript" } else if relative.ends_with(".json") { "application/json" } else if relative.ends_with(".css") { "text/css" } else { "application/octet-stream" };
        response(&mut stream,"200 OK",content_type,&data).await?;
    } else if method=="POST" && path=="/invoke" {
        if request_origin.as_deref()!=Some(&origin) { response(&mut stream,"403 Forbidden","application/json",b"{\"error\":\"untrusted origin\"}").await?; return Ok(()); }
        let input: Value=serde_json::from_slice(&bytes[header_end..header_end+length])?;
        let method=input["method"].as_str().ok_or("invoke method missing")?;
        let result=generation.supervisor.invoke(Invocation { scope_id:&generation.scope,contract_id:&generation.contract,contract_version:generation.version,method,params:input["params"].clone(),grants:&[],deadline:Duration::from_secs(30) }).await;
        match result { Ok(value)=>response(&mut stream,"200 OK","application/json",&serde_json::to_vec(&value)?).await?, Err(error)=>response(&mut stream,"500 Internal Server Error","application/json",&serde_json::to_vec(&json!({"error":error.to_string()}))?).await? }
    } else { response(&mut stream,"404 Not Found","text/plain",b"not found").await?; }
    Ok(())
}
pub struct Server {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Server {
    pub async fn shutdown(mut self) -> std::io::Result<()> {
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
        (&mut self.task).await.map_err(std::io::Error::other)?
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
    }
}
pub async fn start(root: &std::path::Path) -> std::io::Result<(Surface, Server)> {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let host_files = Arc::new(root.join(format!("ui-host-{}",uuid::Uuid::new_v4())));
    let status = std::process::Command::new("node").arg(repository.join("packages/sdk-ui/scripts/build.mjs")).arg("--source").arg(repository.join("tools/module-dev/host")).arg("--output").arg(host_files.as_ref()).stdout(std::process::Stdio::from(std::io::stderr())).status()?;
    if !status.success() { return Err(std::io::Error::other("shared SDK UI host build failed; install workspace UI build dependencies")); }
    let listener=TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,0)).await?;
    let origin=format!("http://{}",listener.local_addr()?);
    println!("Isolated signed worker/UI host: {origin}");
    let current: Surface=Arc::new(RwLock::new(None)); let state=current.clone();
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut requests = tokio::task::JoinSet::new();
        let mut failure = None;
        loop {
            tokio::select! {
                biased;
                _ = &mut stopped => break,
                result = requests.join_next(), if !requests.is_empty() => {
                    if let Some(Err(error)) = result { failure.get_or_insert_with(|| std::io::Error::other(error)); }
                }
                result = listener.accept() => match result {
                    Ok((stream, _)) => {
                        let current = state.clone(); let origin = origin.clone(); let host_files = host_files.clone();
                        requests.spawn(async move {
                            if let Err(error) = tokio::time::timeout(Duration::from_secs(35), serve(stream, current, origin, host_files)).await.unwrap_or_else(|_| Err("dev HTTP deadline exceeded".into())) {
                                eprintln!("dev UI request: {error}");
                            }
                        });
                    }
                    Err(error) => { failure.get_or_insert(error); break; }
                }
            }
        }
        // Close admission before awaiting requests that already own a generation.
        drop(listener);
        while let Some(result) = requests.join_next().await {
            if let Err(error) = result { failure.get_or_insert_with(|| std::io::Error::other(error)); }
        }
        match failure { Some(error) => Err(error), None => Ok(()) }
    });
    Ok((current, Server { stop: Some(stop), task }))
}
