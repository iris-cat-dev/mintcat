use std::{collections::HashMap, sync::RwLock, time::Duration};
use async_trait::async_trait;
use mintcat_runtime_broker::{BrokerError, CancellationToken, CredentialBinding, CredentialPlacement, CredentialRef, HttpMethod, NetworkBroker, NetworkRequest, Origin, ProxyPolicy};
use mintcat_runtime_supervisor::{HostService, HostServiceCaller, SupervisorError};
use mintcat_source_api::{SourceNetworkOutcome, SourceNetworkRequest, SourceNetworkResponse};
use serde_json::Value;
use url::Url;

/// Constructed by the daemon from approved signed active source identity and
/// user settings. Only credential references are retained, never credential bytes.
#[derive(Clone)]
pub struct SourceNetworkPolicy {
    pub module_id: String,
    pub module_digest: String,
    pub source_id: String,
    pub api_base: Url,
    pub website_base: Option<Url>,
    pub credential: Option<CredentialRef>,
    pub proxy_policy: ProxyPolicy,
}
pub struct SourceNetworkHost {
    broker: NetworkBroker,
    policies: RwLock<HashMap<String,SourceNetworkPolicy>>,
}
impl SourceNetworkHost {
    pub fn new(broker:NetworkBroker, policies:Vec<SourceNetworkPolicy>)->Result<Self,SupervisorError> {
        let host=Self {broker,policies:RwLock::new(HashMap::new())};
        for policy in policies {host.approve(policy)?;}
        Ok(host)
    }
    /// Host-only approval of an exact signed identity. Old authorities stay
    /// pinned until their jobs finish; a duplicate digest can never rebind.
    pub fn approve(&self, policy:SourceNetworkPolicy)->Result<(),SupervisorError> {
        if !matches!(policy.source_id.as_str(),"modio"|"modcat"|"nexusmods") || policy.module_id != format!("source-{}",policy.source_id) || !policy.module_digest.starts_with("sha256:") || policy.module_digest.len()!=71 || !policy.module_digest[7..].bytes().all(|byte|byte.is_ascii_hexdigit()) { return Err(SupervisorError::InvalidBinding("source network policy identity is invalid".into())); }
        if !policy.api_base.username().is_empty() || policy.api_base.password().is_some() || policy.website_base.as_ref().is_some_and(|url|!url.username().is_empty() || url.password().is_some()) { return Err(SupervisorError::InvalidBinding("source settings must use credential references, not URL credentials".into())); }
        Origin::from_url(&policy.api_base).map_err(|error|SupervisorError::InvalidBinding(error.to_string()))?;
        if let Some(website)=&policy.website_base { Origin::from_url(website).map_err(|error|SupervisorError::InvalidBinding(error.to_string()))?; }
        let mut approved=self.policies.write().map_err(|_|SupervisorError::InvalidBinding("source policy registry lock poisoned".into()))?;
        if approved.contains_key(&policy.module_digest) {return Err(SupervisorError::InvalidBinding("source digest network policy already approved".into()));}
        approved.insert(policy.module_digest.clone(),policy);
        Ok(())
    }
}
#[async_trait]
impl HostService for SourceNetworkHost {
    async fn execute(&self, caller:HostServiceCaller, method:&str, params:Value)->Result<Value,SupervisorError> {
        if method!="execute" { return Err(SupervisorError::InvalidBinding("unsupported source network method".into())); }
        let policy=self.policies.read().map_err(|_|SupervisorError::InvalidBinding("source policy registry lock poisoned".into()))?
            .get(&caller.module_digest).filter(|policy|policy.module_id==caller.module_id).cloned()
            .ok_or_else(||SupervisorError::InvalidBinding("source worker has no approved network policy".into()))?;
        let input:SourceNetworkRequest=serde_json::from_value(params).map_err(|error|SupervisorError::InvalidBinding(error.to_string()))?;
        let url=Url::parse(&input.url).map_err(|error|SupervisorError::InvalidBinding(error.to_string()))?;
        let api_origin=Origin::from_url(&policy.api_base).map_err(|error|SupervisorError::InvalidBinding(error.to_string()))?;
        let origin=Origin::from_url(&url).map_err(|error|SupervisorError::InvalidBinding(error.to_string()))?;
        let website_origin=policy.website_base.as_ref().map(Origin::from_url).transpose().map_err(|error|SupervisorError::InvalidBinding(error.to_string()))?;
        let is_api=origin==api_origin && url.path().starts_with(policy.api_base.path());
        let is_website=website_origin.as_ref()==Some(&origin) && policy.website_base.as_ref().is_some_and(|base|url.path().starts_with(base.path()));
        if !is_api && !is_website { return Err(SupervisorError::InvalidBinding("source URL is outside the host-configured API scope".into())); }
        if !url.username().is_empty() || url.password().is_some() { return Err(SupervisorError::InvalidBinding("source URL carries forbidden credentials".into())); }
        let http_method=match input.method.as_str() {"GET"=>HttpMethod::Get,"POST" if policy.source_id!="modio"=>HttpMethod::Post,_=>return Err(SupervisorError::InvalidBinding("source HTTP method is not approved".into()))};
        if input.body.as_ref().is_some_and(|body|body.len()>1024*1024) { return Err(SupervisorError::InvalidBinding("source request body exceeds host limit".into())); }
        if input.headers.keys().any(|name|matches!(name.to_ascii_lowercase().as_str(),"authorization"|"apikey"|"cookie"|"host"|"proxy-authorization")) { return Err(SupervisorError::InvalidBinding("source worker cannot supply credential or authority headers".into())); }
        let mut request=NetworkRequest::get(format!("source.{}",policy.source_id),url,vec![origin.clone()]);
        request.method=http_method;
        request.headers=input.headers;
        request.body=input.body.map(Into::into);
        request.proxy_policy=policy.proxy_policy;
        request.timeout=Duration::from_secs(30);
        request.max_response_bytes=32*1024*1024;
        request.accept_http_errors=true;
        if is_api {
            request.credential=policy.credential.as_ref().map(|reference|CredentialBinding { reference:reference.clone(),placement:if policy.source_id=="nexusmods" { CredentialPlacement::Header("apikey".into()) } else { CredentialPlacement::Bearer },allowed_origins:vec![api_origin] });
        }
        let outcome=match self.broker.execute(&request,&CancellationToken::default()).await {
            Ok(response)=>SourceNetworkOutcome::Ready {response:SourceNetworkResponse { request_id:response.request_id,status:response.status,headers:response.headers,body:response.body }},
            Err(BrokerError::CredentialUnavailable(_))=>SourceNetworkOutcome::CredentialUnavailable,
            Err(error)=>return Err(SupervisorError::InvalidBinding(error.to_string())),
        };
        serde_json::to_value(outcome).map_err(|error|SupervisorError::InvalidBinding(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use mintcat_runtime_broker::{CredentialVault,MemoryCredentialStore};
    use serde_json::json;

    fn fixture()->SourceNetworkHost {
        let vault=CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
        SourceNetworkHost::new(NetworkBroker::new(&vault,None).unwrap(),vec![SourceNetworkPolicy {
            module_id:"source-modio".into(),module_digest:format!("sha256:{}","a".repeat(64)),
            source_id:"modio".into(),api_base:Url::parse("http://127.0.0.1:1/v1/").unwrap(),
            website_base:None,credential:None,proxy_policy:ProxyPolicy::Direct,
        }]).unwrap()
    }
    fn caller()->HostServiceCaller {HostServiceCaller {module_id:"source-modio".into(),module_digest:format!("sha256:{}","a".repeat(64))}}
    fn input(url:&str)->Value {json!({"method":"GET","url":url,"headers":{},"body":null,"acceptHttpErrors":false})}

    #[tokio::test]
    async fn network_callbacks_cannot_change_signed_identity_or_origin_authority() {
        let host=fixture();
        let mut wrong_caller=caller();wrong_caller.module_id="source-nexusmods".into();
        assert!(matches!(host.execute(wrong_caller,"execute",input("http://127.0.0.1:1/v1/mods")).await,Err(SupervisorError::InvalidBinding(_))));
        assert!(matches!(host.execute(caller(),"execute",input("http://127.0.0.1:2/v1/mods")).await,Err(SupervisorError::InvalidBinding(_))));
        assert!(matches!(host.execute(caller(),"execute",input("http://127.0.0.1:1/admin")).await,Err(SupervisorError::InvalidBinding(_))));
    }
    #[tokio::test]
    async fn workers_cannot_inject_credentials_or_policy_fields() {
        let host=fixture();
        let mut params=input("http://127.0.0.1:1/v1/mods");
        params["headers"]=json!({"Authorization":"Bearer attacker"});
        assert!(matches!(host.execute(caller(),"execute",params).await,Err(SupervisorError::InvalidBinding(_))));
        let mut params=input("http://127.0.0.1:1/v1/mods");
        params["allowedOrigins"]=json!(["http://127.0.0.1:2"]);
        assert!(matches!(host.execute(caller(),"execute",params).await,Err(SupervisorError::InvalidBinding(_))));
    }
}
