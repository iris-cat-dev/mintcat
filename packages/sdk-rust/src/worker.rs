use std::{future::Future, io, sync::{Arc, atomic::{AtomicU64, Ordering}}};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt}, sync::{mpsc, oneshot}};

use crate::{
    AppError, ProvidedContract, RecoveryAction, WorkerHello, WorkerHostHello, WorkerRequest,
    WorkerRequestKind, WorkerResponse,
};

pub const WORKER_PROTOCOL_VERSION: u64 = 1;
pub const MAX_WORKER_FRAME_BYTES: usize = 1024 * 1024;
pub const WORKER_INSTANCE_ID_ENV: &str = "MINTCAT_WORKER_INSTANCE_ID";
pub const WORKER_MODULE_ID_ENV: &str = "MINTCAT_WORKER_MODULE_ID";
pub const WORKER_MODULE_VERSION_ENV: &str = "MINTCAT_WORKER_MODULE_VERSION";
pub const WORKER_MODULE_DIGEST_ENV: &str = "MINTCAT_WORKER_MODULE_DIGEST";
pub const WORKER_PROVIDES_ENV: &str = "MINTCAT_WORKER_PROVIDES_JSON";
pub const WORKER_WORKSPACE_ENV: &str = "MINTCAT_WORKER_WORKSPACE";

/// Worker callbacks contain no scope or binding authority. The host selects the
/// frozen required provider and authenticates every input and produced descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerBrokerRequest {
    pub id: String,
    pub provider_digest: String,
    pub contract_id: String,
    pub contract_version: u64,
    pub method: String,
    pub params: Value,
    pub grant_ids: Vec<String>,
    pub produced: Vec<crate::ArtifactGrantDescriptor>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkerOutbound {
    Broker { broker: WorkerBrokerRequest },
    Response(WorkerResponse),
}

struct BrokerCall {
    request: WorkerBrokerRequest,
    response: oneshot::Sender<Result<Value, WorkerProtocolError>>,
}

#[derive(Clone)]
pub struct WorkerBroker {
    sender: mpsc::Sender<BrokerCall>,
    sequence: Arc<AtomicU64>,
}

impl WorkerBroker {
    fn identify(&self, request: &mut WorkerBrokerRequest) {
        request.id = format!("broker-{}", self.sequence.fetch_add(1, Ordering::Relaxed));
    }

    pub async fn invoke(&self, mut request: WorkerBrokerRequest) -> Result<Value, WorkerProtocolError> {
        self.identify(&mut request);
        let (response, receive) = oneshot::channel();
        self.sender.send(BrokerCall { request, response }).await
            .map_err(|_| WorkerProtocolError::Rejected("broker invocation is no longer active".into()))?;
        receive.await.map_err(|_| WorkerProtocolError::Rejected("broker response channel closed".into()))?
    }

    /// Only call from a blocking worker thread, never a Tokio executor thread.
    pub fn blocking_invoke(&self, mut request: WorkerBrokerRequest) -> Result<Value, WorkerProtocolError> {
        self.identify(&mut request);
        let (response, receive) = oneshot::channel();
        self.sender.blocking_send(BrokerCall { request, response })
            .map_err(|_| WorkerProtocolError::Rejected("broker invocation is no longer active".into()))?;
        receive.blocking_recv().map_err(|_| WorkerProtocolError::Rejected("broker response channel closed".into()))?
    }
}

#[derive(Debug, Clone)]
pub struct WorkerIdentity {
    pub instance_id: String,
    pub module_id: String,
    pub module_version: String,
    pub module_digest: String,
    pub provides: Vec<ProvidedContract>,
}

impl WorkerIdentity {
    pub fn from_env() -> Result<Self, WorkerProtocolError> {
        fn required(name: &str) -> Result<String, WorkerProtocolError> {
            std::env::var(name).map_err(|_| {
                WorkerProtocolError::Environment(format!(
                    "required worker environment variable {name} is absent"
                ))
            })
        }

        let provides = serde_json::from_str(&required(WORKER_PROVIDES_ENV)?)?;
        Ok(Self {
            instance_id: required(WORKER_INSTANCE_ID_ENV)?,
            module_id: required(WORKER_MODULE_ID_ENV)?,
            module_version: required(WORKER_MODULE_VERSION_ENV)?,
            module_digest: required(WORKER_MODULE_DIGEST_ENV)?,
            provides,
        })
    }
}

#[derive(Debug, Error)]
pub enum WorkerProtocolError {
    #[error("worker protocol I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("worker protocol JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("worker environment is invalid: {0}")]
    Environment(String),
    #[error("worker protocol frame is {actual} bytes; maximum is {MAX_WORKER_FRAME_BYTES}")]
    FrameTooLarge { actual: usize },
    #[error("worker handshake was rejected: {0}")]
    Rejected(String),
    #[error("worker protocol {actual} is incompatible with {WORKER_PROTOCOL_VERSION}")]
    IncompatibleProtocol { actual: u64 },
}

pub async fn write_worker_message<W, T>(
    writer: &mut W,
    value: &T,
) -> Result<(), WorkerProtocolError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_WORKER_FRAME_BYTES {
        return Err(WorkerProtocolError::FrameTooLarge {
            actual: payload.len(),
        });
    }
    let length = u32::try_from(payload.len()).expect("worker frame limit fits in u32");
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_worker_message<R, T>(reader: &mut R) -> Result<T, WorkerProtocolError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix).await?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAX_WORKER_FRAME_BYTES {
        return Err(WorkerProtocolError::FrameTooLarge { actual: length });
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(serde_json::from_slice(&payload)?)
}

pub fn worker_app_error(
    request_id: impl Into<String>,
    code: impl Into<String>,
    message_key: impl Into<String>,
    details: Value,
) -> Box<AppError> {
    Box::new(AppError {
        code: code.into(),
        message_key: message_key.into(),
        details,
        request_id: request_id.into(),
        job_id: None,
        retryable: false,
        recovery_actions: Vec::<RecoveryAction>::new(),
    })
}

pub async fn run_worker<H, Fut>(
    identity: WorkerIdentity,
    handler: H,
) -> Result<(), WorkerProtocolError>
where
    H: Fn(WorkerRequest) -> Fut,
    Fut: Future<Output = Result<Value, Box<AppError>>>,
{
    run_worker_with_broker(identity, move |request, _broker| handler(request)).await
}

pub async fn run_worker_with_broker<H, Fut>(
    identity: WorkerIdentity,
    handler: H,
) -> Result<(), WorkerProtocolError>
where
    H: Fn(WorkerRequest, WorkerBroker) -> Fut,
    Fut: Future<Output = Result<Value, Box<AppError>>>,
{
    let mut input = tokio::io::stdin();
    let mut output = tokio::io::stdout();
    write_worker_message(
        &mut output,
        &WorkerHello {
            protocol_version: WORKER_PROTOCOL_VERSION,
            instance_id: identity.instance_id.clone(),
            module_id: identity.module_id,
            module_version: identity.module_version,
            module_digest: identity.module_digest,
            provides: identity.provides,
        },
    )
    .await?;

    let host: WorkerHostHello = read_worker_message(&mut input).await?;
    if host.protocol_version != WORKER_PROTOCOL_VERSION {
        return Err(WorkerProtocolError::IncompatibleProtocol {
            actual: host.protocol_version,
        });
    }
    if !host.accepted {
        return Err(WorkerProtocolError::Rejected(
            host.error
                .unwrap_or_else(|| "host supplied no reason".to_owned()),
        ));
    }

    loop {
        let request: WorkerRequest = read_worker_message(&mut input).await?;
        let shutdown = request.kind == WorkerRequestKind::Shutdown;
        let outcome = match request.kind {
            WorkerRequestKind::Health => Ok(json!({
                "status": "healthy",
                "instanceId": identity.instance_id,
            })),
            WorkerRequestKind::Invoke => {
                let (sender, mut calls) = mpsc::channel::<BrokerCall>(8);
                let broker = WorkerBroker { sender, sequence: Arc::new(AtomicU64::new(1)) };
                let future = handler(request.clone(), broker);
                tokio::pin!(future);
                loop {
                    tokio::select! {
                        outcome = &mut future => break outcome,
                        Some(call) = calls.recv() => {
                            write_worker_message(&mut output, &WorkerOutbound::Broker { broker: call.request.clone() }).await?;
                            let response: WorkerResponse = read_worker_message(&mut input).await?;
                            if response.id != call.request.id {
                                return Err(WorkerProtocolError::Rejected("broker response id mismatch".into()));
                            }
                            let result = if response.ok {
                                response.result.ok_or_else(|| WorkerProtocolError::Rejected("broker result absent".into()))
                            } else {
                                Err(WorkerProtocolError::Rejected(
                                    response.error.map(|error| format!("{}: {} ({})", error.code, error.message_key, error.details))
                                        .unwrap_or_else(|| "broker error absent".into()),
                                ))
                            };
                            let _ = call.response.send(result);
                        }
                    }
                }
            },
            WorkerRequestKind::Shutdown => Ok(json!({ "drained": true })),
        };
        let response = match outcome {
            Ok(result) => WorkerResponse {
                id: request.id,
                ok: true,
                result: Some(result),
                error: None,
            },
            Err(error) => WorkerResponse {
                id: request.id,
                ok: false,
                result: None,
                error: Some(*error),
            },
        };
        write_worker_message(&mut output, &response).await?;
        if shutdown {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        read_worker_message, write_worker_message, WorkerProtocolError, MAX_WORKER_FRAME_BYTES,
    };

    #[tokio::test]
    async fn rejects_an_oversized_frame_before_payload_allocation() {
        let mut input = Cursor::new(
            u32::try_from(MAX_WORKER_FRAME_BYTES + 1)
                .unwrap()
                .to_be_bytes(),
        );
        let error = read_worker_message::<_, serde_json::Value>(&mut input)
            .await
            .unwrap_err();
        assert!(matches!(error, WorkerProtocolError::FrameTooLarge { .. }));
    }

    #[tokio::test]
    async fn round_trips_a_framed_worker_message() {
        let mut bytes = Cursor::new(Vec::new());
        write_worker_message(&mut bytes, &serde_json::json!({ "ok": true }))
            .await
            .unwrap();
        bytes.set_position(0);
        let decoded: serde_json::Value = read_worker_message(&mut bytes).await.unwrap();
        assert_eq!(decoded, serde_json::json!({ "ok": true }));
    }
}
