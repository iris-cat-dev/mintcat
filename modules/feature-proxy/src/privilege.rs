use super::ProxyServiceManager;
use anyhow::{Context, Result};
use mintcat_runtime_jobs::{ActionBinding, ActionSpec};
use mintcat_sdk::{ActionRequired, JobRef};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PrivilegeOperation {
    CreateCredentials,
    InstallCertificate,
    RemoveCertificate,
    ApplyHosts,
    CleanupHosts,
    AuthorizeListenPort { port: u16 },
}
impl ProxyServiceManager {
    /// Jobs must use Required confirmation; certificate/hosts/credential grants are distinct actions.
    pub fn request_privilege(&self, job_id: &str, digest: &str, operation: PrivilegeOperation, expires_at_ms: i64) -> Result<ActionRequired> {
        let job = self.jobs.get_job(&self.owner_scope, job_id)?;
        anyhow::ensure!(job.command == "proxy.privilege", "job does not belong to proxy privilege authority");
        let target = self.jobs.get_job_target(&self.owner_scope, job_id)?;
        anyhow::ensure!(target.get("digest") == Some(&json!(digest)), "proxy privilege digest differs from durable job target");
        if job.state == mintcat_sdk::JobState::Queued {
            self.jobs.append_event(job_id, mintcat_runtime_jobs::EventUpdate::state("proxy-privilege-preparing", mintcat_sdk::JobState::Preparing))?;
        } else {
            anyhow::ensure!(job.state == mintcat_sdk::JobState::Preparing, "proxy privilege job already has an action or requires recovery");
        }
        anyhow::ensure!(!self.revoked.contains(digest) || operation == PrivilegeOperation::CleanupHosts || operation == PrivilegeOperation::RemoveCertificate, "revoked proxy cannot request new privilege");
        let lease = self.artifacts.acquire_module(digest)?;
        anyhow::ensure!(lease.module().manifest.id == super::MODULE_ID, "not a proxy artifact");
        let binding = self.privilege_binding(digest, &operation)?;
        Ok(self.jobs.create_action(job_id, ActionSpec {
            kind: "proxy-system-operation".into(), operation_id: format!("proxy:{job_id}"),
            target: binding.target, target_revision: binding.target_revision, plan_hash: binding.plan_hash,
            module_lock_digest: binding.module_lock_digest, expires_at_ms,
            response_schema: json!({"type":"object","properties":{"approved":{"type":"boolean","const":true}},"required":["approved"],"additionalProperties":false}),
        })?)
    }
    /// This is a restricted broker: fixed system tools and daemon-owned data only, no shell/executable/path input.
    pub fn execute_privilege(&mut self, action_id: &str, digest: &str, operation: PrivilegeOperation) -> Result<JobRef> {
        let action = self.jobs.get_action(&self.owner_scope, action_id)?;
        anyhow::ensure!(action.kind == "proxy-system-operation" && action.target.get("moduleId") == Some(&json!(super::MODULE_ID)) && action.target.get("digest") == Some(&json!(digest)) && action.target.get("operation") == Some(&json!(&operation)), "action does not belong to this proxy artifact and operation");
        let job_id = action.operation_id.strip_prefix("proxy:").context("proxy action operation identity missing")?;
        let snapshot = self.jobs.get_job(&self.owner_scope, job_id)?;
        anyhow::ensure!(snapshot.command == "proxy.privilege", "action job does not belong to proxy privilege authority");
        if snapshot.state == mintcat_sdk::JobState::Succeeded {
            anyhow::ensure!(action.state == mintcat_sdk::ActionState::Consumed, "successful privilege job lacks consumed approval");
            return Ok(JobRef { job_id: job_id.into() });
        }
        anyhow::ensure!(!self.revoked.contains(digest) || operation == PrivilegeOperation::CleanupHosts || operation == PrivilegeOperation::RemoveCertificate, "revoked proxy cannot gain new privilege");
        let binding = self.privilege_binding(digest, &operation)?;
        let consumed = self.jobs.consume_action(action_id, &binding)?;
        anyhow::ensure!(consumed.response == json!({"approved":true}), "proxy operation requires independent positive approval");
        let snapshot = self.jobs.get_job(&self.owner_scope, &consumed.job.job_id)?;
        anyhow::ensure!(snapshot.state == mintcat_sdk::JobState::Running, "proxy system operation has an unknown or recovered commit; inspect it instead of repeating privilege");
        self.record_write()?;
        self.jobs.begin_commit(&consumed.job.job_id)?;
        if operation == PrivilegeOperation::CleanupHosts { self.cleanup_action_id = Some(action_id.into()); }
        let result = self.perform_operation(digest, &operation);
        match result {
            Ok(()) => { self.jobs.finish_commit(&consumed.job.job_id, json!({"operation":operation}))?; Ok(consumed.job) }
            Err(error) => {
                // Partial system mutation must be inspected, not reported as a successful no-op or retried blindly.
                self.jobs.mark_commit_unknown(&consumed.job.job_id)?;
                Err(error.context("proxy system authorization/operation failed; durable job requires inspection"))
            }
        }
    }
    fn privilege_binding(&self, digest: &str, operation: &PrivilegeOperation) -> Result<ActionBinding> {
        let files = ["ca.pem", "ca.key", "accelerate_cache.json", "custom_rules.json"];
        let mut state = serde_json::Map::new();
        for file in files {
            let path = self.data_dir.join(file);
            if path.exists() {
                let metadata = std::fs::symlink_metadata(&path)?;
                anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink(), "proxy credential/rules must be owned regular files");
                state.insert(file.into(), Value::String(format!("sha256:{:x}", Sha256::digest(std::fs::read(path)?))));
            } else { state.insert(file.into(), Value::Null); }
        }
        let target = json!({"moduleId":super::MODULE_ID,"digest":digest,"operation":operation,"state":state});
        let plan_hash = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&target)?));
        Ok(ActionBinding { target, target_revision: 0, plan_hash, module_lock_digest: digest.into() })
    }
    fn perform_operation(&mut self, digest: &str, operation: &PrivilegeOperation) -> Result<()> {
        match operation {
            PrivilegeOperation::CreateCredentials => {
                anyhow::ensure!(!self.data_dir.join("ca.pem").exists() && !self.data_dir.join("ca.key").exists(), "proxy CA already exists or is incomplete; do not overwrite credentials");
                super::cert::CertManager::new_or_load(&self.data_dir)?;
                Ok(())
            }
            PrivilegeOperation::InstallCertificate | PrivilegeOperation::RemoveCertificate => {
                anyhow::ensure!(self.data_dir.join("ca.pem").is_file() && self.data_dir.join("ca.key").is_file(), "proxy CA credentials missing");
                let cert = super::cert::CertManager::new_or_load(&self.data_dir)?;
                if *operation == PrivilegeOperation::InstallCertificate { cert.install_ca_trust() } else { cert.uninstall_ca_trust() }
            }
            PrivilegeOperation::ApplyHosts => {
                anyhow::ensure!(self.cleanup_authorized, "hosts integration requires a separately approved cleanup operation first");
                let running = self.running.as_ref().context("hosts integration requires a ready owned proxy")?;
                anyhow::ensure!(running.identity.module_digest == digest && running.address.port() == 443 && running.address.ip() == "127.0.0.1".parse::<std::net::IpAddr>()?, "hosts integration requires this leased artifact on 127.0.0.1:443");
                let groups = super::config::WattApiClient::load_cache(&self.data_dir)?;
                let mut configs = super::config::flatten_projects(&groups);
                configs.extend(super::config::load_custom_rules(&self.data_dir));
                let domains: Vec<_> = configs.into_iter().flat_map(|config| config.listen_domains).filter(|domain| !domain.contains('*')).map(|domain| (domain, "127.0.0.1".into())).collect();
                anyhow::ensure!(!domains.is_empty(), "no exact domains available for hosts integration");
                let cleanup = self.cleanup_action_id.as_ref().context("durable cleanup action missing")?;
                std::fs::write(self.data_dir.join("hosts-owner.json"), serde_json::to_vec(cleanup)?)?;
                self.owned_hosts = true;
                super::hosts::update_hosts(&domains)?;
                Ok(())
            }
            PrivilegeOperation::AuthorizeListenPort { port } => {
                anyhow::ensure!(*port > 0 && *port < 1024, "listen authorization is only for privileged ports");
                // This authorizes the subsequent actual bind. OS refusal still propagates through failed-ready cleanup.
                self.authorized_ports.insert((digest.into(), *port));
                Ok(())
            }
            PrivilegeOperation::CleanupHosts => {
                super::hosts::cleanup_hosts()?;
                self.cleanup_authorized = true;
                self.owned_hosts = false;
                let receipt = self.data_dir.join("hosts-owner.json");
                if receipt.exists() { std::fs::remove_file(receipt)?; }
                Ok(())
            }
        }
    }
}
