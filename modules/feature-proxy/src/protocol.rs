use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub const MODULE_ID: &str = "mintcat.feature.proxy";
pub const SERVICE_PROTOCOL: &str = "mintcat.proxy-service/1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartOptions {
    pub port: u16,
    pub offline: bool,
    pub bind: IpAddr,
}
impl Default for StartOptions {
    fn default() -> Self {
        Self { port: 8443, offline: false, bind: IpAddr::V4(Ipv4Addr::LOCALHOST) }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceIdentity {
    pub protocol: String,
    pub module_digest: String,
    pub module_version: String,
    pub instance_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceStatus {
    pub running: bool,
    pub identity: Option<ServiceIdentity>,
    pub address: Option<SocketAddr>,
    pub pid: Option<u32>,
}
impl ServiceStatus {
    pub fn stopped() -> Self {
        Self { running: false, identity: None, address: None, pid: None }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ServiceRequest { Health { identity: ServiceIdentity }, Stop { identity: ServiceIdentity } }
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceReply {
    pub identity: ServiceIdentity,
    pub state: String,
    pub address: SocketAddr,
}

pub fn validate_reply(expected: &ServiceIdentity, reply: &ServiceReply, state: &str) -> anyhow::Result<()> {
    anyhow::ensure!(reply.identity == *expected, "proxy service identity/protocol mismatch");
    anyhow::ensure!(reply.state == state, "proxy service state mismatch: {}", reply.state);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_cannot_adopt_another_artifact_version_protocol_or_process() {
        let identity = ServiceIdentity {
            protocol: SERVICE_PROTOCOL.into(), module_digest: "sha256:old".into(),
            module_version: "1.0.0".into(), instance_id: "owned-child".into(),
        };
        for foreign in [
            ServiceIdentity { protocol: "mintcat.proxy-service/2".into(), ..identity.clone() },
            ServiceIdentity { module_digest: "sha256:new".into(), ..identity.clone() },
            ServiceIdentity { module_version: "2.0.0".into(), ..identity.clone() },
            ServiceIdentity { instance_id: "unowned-child".into(), ..identity.clone() },
        ] {
            let reply = ServiceReply { identity: foreign, state: "ready".into(), address: "127.0.0.1:8443".parse().unwrap() };
            assert!(validate_reply(&identity, &reply, "ready").unwrap_err().to_string().contains("identity/protocol mismatch"));
        }
        let reply = ServiceReply { identity, state: "starting".into(), address: "127.0.0.1:8443".parse().unwrap() };
        assert!(validate_reply(&reply.identity, &reply, "ready").unwrap_err().to_string().contains("state mismatch"));
    }
}
