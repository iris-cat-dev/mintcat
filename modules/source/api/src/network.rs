use std::collections::BTreeMap;
use mintcat_runtime_broker::{BrokerError, CancellationToken, HttpMethod, NetworkBroker, NetworkRequest, NetworkResponse, RouteKind};
use mintcat_sdk::{WorkerBroker, WorkerBrokerRequest};
use serde::{Deserialize, Serialize};

pub const SOURCE_NETWORK_CONTRACT: &str = "source.network";
pub const HOST_PROVIDER_DIGEST: &str = "host:source.network";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct SourceNetworkRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String,String>,
    pub body: Option<Vec<u8>>,
    pub accept_http_errors: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct SourceNetworkResponse {
    pub request_id: String,
    pub status: u16,
    pub headers: BTreeMap<String,String>,
    pub body: Vec<u8>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag="state",rename_all="kebab-case",deny_unknown_fields)]
pub enum SourceNetworkOutcome {
    Ready { response: SourceNetworkResponse },
    CredentialUnavailable,
}
#[derive(Clone)]
pub enum SourceNetworkClient { Host(NetworkBroker), Worker(WorkerBroker) }
impl From<NetworkBroker> for SourceNetworkClient { fn from(broker:NetworkBroker)->Self { Self::Host(broker) } }
impl From<WorkerBroker> for SourceNetworkClient { fn from(broker:WorkerBroker)->Self { Self::Worker(broker) } }
impl SourceNetworkClient {
    pub async fn execute(&self, request:&NetworkRequest,cancellation:&CancellationToken)->Result<NetworkResponse,BrokerError> {
        match self {
            Self::Host(broker)=>broker.execute(request,cancellation).await,
            Self::Worker(broker)=> {
                let method = match request.method { HttpMethod::Get=>"GET",HttpMethod::Post=>"POST",HttpMethod::Put=>"PUT",HttpMethod::Patch=>"PATCH",HttpMethod::Delete=>"DELETE",HttpMethod::Head=>"HEAD" };
                let params = serde_json::json!({"method":method,"url":request.url.as_str(),"headers":&request.headers,"body":request.body.as_deref(),"acceptHttpErrors":request.accept_http_errors});
                let result = tokio::select! {
                    biased;
                    ()=cancellation.cancelled()=>return Err(BrokerError::Cancelled { request_id:"source-worker".into() }),
                    result=broker.invoke(WorkerBrokerRequest { id:String::new(),provider_digest:HOST_PROVIDER_DIGEST.into(),contract_id:SOURCE_NETWORK_CONTRACT.into(),contract_version:1,method:"execute".into(),params,grant_ids:vec![],produced:vec![] })=>result.map_err(|error|BrokerError::InvalidRequest(error.to_string()))?,
                };
                let outcome:SourceNetworkOutcome=serde_json::from_value(result).map_err(|error|BrokerError::InvalidRequest(error.to_string()))?;
                let response=match outcome {
                    SourceNetworkOutcome::Ready {response}=>response,
                    SourceNetworkOutcome::CredentialUnavailable=>return Err(BrokerError::CredentialUnavailable("approved source credential".into())),
                };
                if response.status >= 400 && !request.accept_http_errors {
                    return Err(BrokerError::Http { request_id:response.request_id,status:response.status,endpoint:format!("{}{}",request.url.origin().ascii_serialization(),request.url.path()) });
                }
                Ok(NetworkResponse { request_id:response.request_id,status:response.status,headers:response.headers,body:response.body,route:RouteKind::Direct })
            }
        }
    }
}
