pub mod cert;
pub mod config;
mod hosts;
pub mod protocol;
mod privilege;
pub use protocol::{ServiceIdentity, ServiceStatus, StartOptions, MODULE_ID, SERVICE_PROTOCOL};
pub use privilege::PrivilegeOperation;

use anyhow::{Context, Result};
use mintcat_runtime_artifacts::{ArtifactStore, ModuleLease};
use mintcat_runtime_jobs::JobStore;
use std::{collections::BTreeSet, path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::{Child, ChildStdin, ChildStdout, Command}};

const DEADLINE: Duration = Duration::from_secs(5);

pub struct ProxyServiceManager {
    artifacts: ArtifactStore,
    jobs: JobStore,
    owner_scope: String,
    data_dir: PathBuf,
    revoked: BTreeSet<String>,
    authorized_ports: BTreeSet<(String, u16)>,
    owned_hosts: bool,
    cleanup_authorized: bool,
    cleanup_action_id: Option<String>,
    running: Option<RunningService>,
    write_epoch: u64,
}
struct RunningService {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    identity: ServiceIdentity,
    address: std::net::SocketAddr,
    _lease: ModuleLease,
}
impl ProxyServiceManager {
    /// Construct in the daemon, never a GUI session or unloadable UI contribution.
    pub fn new(artifacts: ArtifactStore, jobs: JobStore, data_dir: PathBuf) -> Result<Self> {
        Self::new_for_owner(artifacts, jobs, data_dir, MODULE_ID)
    }
    pub fn new_for_owner(artifacts: ArtifactStore, jobs: JobStore, data_dir: PathBuf, owner_scope: impl Into<String>) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut manager = Self { artifacts, jobs, owner_scope: owner_scope.into(), data_dir, revoked: BTreeSet::new(), running: None, authorized_ports: BTreeSet::new(), owned_hosts: false, cleanup_authorized: false, cleanup_action_id: None, write_epoch: 0 };
        let epoch = manager.data_dir.join("write-epoch.json");
        if epoch.exists() {
            let metadata = std::fs::symlink_metadata(&epoch)?;
            anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink(), "proxy epoch must be owned regular metadata");
            manager.write_epoch = serde_json::from_slice(&std::fs::read(epoch)?)?;
        }
        let revocations = manager.data_dir.join("revoked-artifacts.json");
        if revocations.exists() {
            let metadata = std::fs::symlink_metadata(&revocations)?;
            anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink(), "proxy revocations must be owned regular metadata");
            manager.revoked = serde_json::from_slice(&std::fs::read(revocations)?)?;
        }
        let receipt = manager.data_dir.join("hosts-owner.json");
        if receipt.exists() {
            let metadata = std::fs::symlink_metadata(&receipt)?;
            anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink(), "hosts ownership receipt must be regular");
            let action_id: String = serde_json::from_slice(&std::fs::read(&receipt)?)?;
            let action = manager.jobs.get_action(&manager.owner_scope, &action_id)?;
            anyhow::ensure!(action.state == mintcat_sdk::ActionState::Consumed && action.kind == "proxy-system-operation" && action.target.pointer("/operation/operation") == Some(&serde_json::json!("cleanup-hosts")), "hosts ownership lacks durable cleanup authorization");
            manager.cleanup_action_id = Some(action_id);
            manager.cleanup_authorized = true;
            manager.owned_hosts = true;
            manager.cleanup_owned_hosts()?;
        }
        Ok(manager)
    }
    pub fn has_persistent_state(&self) -> bool {
        ["ca.pem", "ca.key", "accelerate_cache.json", "custom_rules.json", "hosts-owner.json"].iter().any(|file| self.data_dir.join(file).exists())
    }
    /// Trust is supplied only by the daemon's verified signed scope, never request parameters.
    pub fn set_artifact_store(&mut self, artifacts: ArtifactStore) { self.artifacts = artifacts; }
    pub fn owns_service(&self) -> bool { self.running.is_some() }
    pub fn write_epoch(&self) -> u64 { self.write_epoch }
    /// Portable configuration only. CA material, host receipts and process identity
    /// remain machine-local authority and are never read into this snapshot.
    pub fn portable_snapshot(&self) -> Result<std::collections::BTreeMap<String, serde_json::Value>> {
        anyhow::ensure!(!self.owns_service(), "active Proxy service blocks a portable snapshot");
        let mut values=std::collections::BTreeMap::new();
        for name in ["custom_rules.json", "accelerate_cache.json"] {
            let path=self.data_dir.join(name);
            match std::fs::symlink_metadata(&path) {
                Ok(metadata)=>{
                    anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink(), "Proxy configuration must be a regular owner file");
                    values.insert(name.into(),serde_json::from_slice(&std::fs::read(path)?)?);
                },
                Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},
                Err(error)=>return Err(error.into()),
            }
        }
        Self::validate_portable(&values)?;
        Ok(values)
    }
    pub fn validate_portable(values:&std::collections::BTreeMap<String,serde_json::Value>)->Result<()> {
        for (name,value) in values {
            match name.as_str() {
                "custom_rules.json"=>{let _:Vec<config::CustomRule>=serde_json::from_value(value.clone())?;},
                "accelerate_cache.json"=>{let _:Vec<config::AccelerateProjectGroup>=serde_json::from_value(value.clone())?;},
                _=>anyhow::bail!("unsupported portable Proxy field"),
            }
        }
        Ok(())
    }
    pub fn ensure_empty_portable_target(&self)->Result<()> {
        anyhow::ensure!(!self.owns_service() && !self.has_persistent_state() && self.write_epoch==0, "Proxy import requires an empty, never-written target");
        Ok(())
    }
    pub fn import_empty_portable(&mut self, values:&std::collections::BTreeMap<String,serde_json::Value>, expected_epoch:u64)->Result<()> {
        self.ensure_empty_portable_target()?;
        anyhow::ensure!(self.write_epoch==expected_epoch, "Proxy target write fence changed");
        Self::validate_portable(values)?;
        if values.is_empty() {return Ok(());}
        // The common maintenance marker records ambiguous/partial multi-file commit.
        // Advance the owner fence first: recovery must inspect, never overwrite.
        self.record_write()?;
        for (name,value) in values {
            let mut options=std::fs::OpenOptions::new(); options.write(true).create_new(true);
            #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt; options.mode(0o600);}
            let mut file=options.open(self.data_dir.join(name))?;
            use std::io::Write;
            file.write_all(&serde_json::to_vec(value)?)?; file.sync_all()?;
        }
        #[cfg(unix)] std::fs::File::open(&self.data_dir)?.sync_all()?;
        Ok(())
    }
    fn record_write(&mut self) -> Result<()> {
        let epoch = self.write_epoch.checked_add(1).context("proxy write epoch exhausted")?;
        let temporary = self.data_dir.join(format!("epoch-{}.tmp", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec(&epoch)?)?;
        file.sync_all()?;
        std::fs::rename(temporary, self.data_dir.join("write-epoch.json"))?;
        #[cfg(unix)]
        std::fs::File::open(&self.data_dir)?.sync_all()?;
        self.write_epoch = epoch;
        Ok(())
    }
    pub fn revoke(&mut self, digest: String) -> Result<()> {
        if !self.revoked.contains(&digest) { self.record_write()?; }
        self.revoked.insert(digest);
        let temporary = self.data_dir.join(format!("revocations-{}.tmp", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec(&self.revoked)?)?;
        file.sync_all()?;
        std::fs::rename(temporary, self.data_dir.join("revoked-artifacts.json"))?;
        #[cfg(unix)]
        std::fs::File::open(&self.data_dir)?.sync_all()?;
        Ok(())
    }
    pub fn status(&mut self) -> Result<ServiceStatus> {
        if let Some(service) = self.running.as_mut() {
            if service.child.try_wait()?.is_none() {
                return Ok(ServiceStatus { running: true, identity: Some(service.identity.clone()), address: Some(service.address), pid: service.child.id() });
            }
        }
        self.running = None;
        self.cleanup_owned_hosts()?;
        Ok(ServiceStatus::stopped())
    }
    pub async fn start(&mut self, digest: &str, options: StartOptions) -> Result<ServiceStatus> {
        anyhow::ensure!(!self.revoked.contains(digest), "proxy artifact is revoked for new starts");
        let current = self.status()?;
        if let Some(identity) = &current.identity {
            anyhow::ensure!(identity.module_digest == digest, "another proxy artifact already owns the service; stop it with its leased protocol first");
            return self.health().await;
        }
        anyhow::ensure!(options.bind.is_loopback(), "proxy service must bind loopback");
        anyhow::ensure!(options.port == 0 || options.port >= 1024 || self.authorized_ports.contains(&(digest.into(), options.port)), "privileged listen port needs independent durable approval");
        let lease = self.artifacts.acquire_module(digest)?;
        let module = lease.module();
        anyhow::ensure!(module.manifest.id == MODULE_ID, "artifact is not the proxy module");
        let component = module.manifest.components.iter().find(|component| component.kind == "service" && component.runtime == SERVICE_PROTOCOL)
            .context("signed proxy service component/protocol missing")?;
        anyhow::ensure!(module.manifest.files.iter().any(|file| file.path == component.entry && file.executable), "proxy service entry is not signed executable content");
        let executable = module.entry_path(&component.entry)?;
        self.record_write()?;
        let identity = ServiceIdentity { protocol: SERVICE_PROTOCOL.into(), module_digest: module.digest.clone(), module_version: module.manifest.version.clone(), instance_id: format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()) };
        let log = std::fs::OpenOptions::new().create(true).append(true).open(self.data_dir.join("service.log"))?;
        let mut command = Command::new(executable);
        command.env_clear().env("MINTCAT_PROXY_DIGEST", &identity.module_digest)
            .env("MINTCAT_PROXY_VERSION", &identity.module_version).env("MINTCAT_PROXY_INSTANCE", &identity.instance_id)
            .arg("--data-dir").arg(&self.data_dir).arg("--bind").arg(options.bind.to_string()).arg("--port").arg(options.port.to_string())
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::from(log)).kill_on_drop(true);
        if options.offline { command.arg("--offline"); }
        let mut child = command.spawn().context("proxy service spawn failed")?;
        let input = child.stdin.take().context("proxy control pipe unavailable")?;
        let output = BufReader::new(child.stdout.take().context("proxy response pipe unavailable")?);
        let mut service = RunningService { child, input, output, identity, address: (options.bind, options.port).into(), _lease: lease };
        let readiness = service.read_reply("ready").await;
        match readiness {
            Ok(reply) => {
                if reply.address.ip() != options.bind || (options.port != 0 && reply.address.port() != options.port) {
                    service.terminate().await?;
                    self.cleanup_owned_hosts()?;
                    anyhow::bail!("proxy ready listener differs from approved start");
                }
                service.address = reply.address;
                self.running = Some(service);
                self.health().await
            }
            Err(error) => {
                service.terminate().await?;
                self.cleanup_owned_hosts()?;
                Err(error.context("proxy failed readiness; owned child terminated and reaped"))
            }
        }
    }
    pub async fn health(&mut self) -> Result<ServiceStatus> {
        let Some(service) = self.running.as_mut() else { return Ok(ServiceStatus::stopped()) };
        let result = service.exchange(protocol::ServiceRequest::Health { identity: service.identity.clone() }, "healthy").await;
        if let Err(error) = result {
            service.terminate().await?;
            self.running = None;
            self.cleanup_owned_hosts()?;
            return Err(error.context("proxy health failed; owned child cleaned up"));
        }
        self.status()
    }
    /// Never consult active module selection, revocation, or a newer binary when stopping.
    pub async fn stop(&mut self) -> Result<ServiceStatus> {
        if self.running.is_some() || self.owned_hosts { self.record_write()?; }
        let Some(mut service) = self.running.take() else {
            self.cleanup_owned_hosts()?;
            return Ok(ServiceStatus::stopped());
        };
        let result = service.exchange(protocol::ServiceRequest::Stop { identity: service.identity.clone() }, "stopped").await;
        if result.is_ok() {
            if let Ok(waited) = tokio::time::timeout(DEADLINE, service.child.wait()).await {
                let exited = waited?;
                self.cleanup_owned_hosts()?;
                anyhow::ensure!(exited.success(), "proxy stop exited unsuccessfully");
                return Ok(ServiceStatus::stopped());
            }
        }
        service.terminate().await?;
        self.cleanup_owned_hosts()?;
        result?;
        Ok(ServiceStatus::stopped())
    }
    fn cleanup_owned_hosts(&mut self) -> Result<()> {
        if self.owned_hosts {
            anyhow::ensure!(self.cleanup_authorized, "owned hosts cleanup authorization missing");
            self.record_write()?;
            hosts::cleanup_hosts()?;
            let receipt = self.data_dir.join("hosts-owner.json");
            if receipt.exists() { std::fs::remove_file(receipt)?; }
            self.owned_hosts = false;
        }
        Ok(())
    }
}
impl RunningService {
    async fn read_reply(&mut self, state: &str) -> Result<protocol::ServiceReply> {
        let mut bytes = Vec::new();
        use tokio::io::AsyncReadExt;
        // A bounded reader prevents a corrupt signed service from allocating indefinitely.
        let count = tokio::time::timeout(DEADLINE, (&mut self.output).take(4097).read_until(b'\n', &mut bytes)).await.context("proxy response deadline expired")??;
        anyhow::ensure!(count > 0 && count <= 4096 && bytes.last() == Some(&b'\n'), "proxy control EOF or oversized response");
        let reply = serde_json::from_slice(&bytes)?;
        protocol::validate_reply(&self.identity, &reply, state)?;
        Ok(reply)
    }
    async fn exchange(&mut self, request: protocol::ServiceRequest, state: &str) -> Result<protocol::ServiceReply> {
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        tokio::time::timeout(DEADLINE, self.input.write_all(&bytes)).await.context("proxy request deadline expired")??;
        self.read_reply(state).await
    }
    async fn terminate(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill().await.context("cannot terminate owned proxy child")?;
        }
        self.child.wait().await.context("cannot reap owned proxy child")?;
        Ok(())
    }
}
