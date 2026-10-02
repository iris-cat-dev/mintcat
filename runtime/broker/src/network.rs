use bytes::Bytes;
use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use futures_util::StreamExt as _;
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, LOCATION},
    Client, Method,
};
use tokio::sync::Notify;
use url::Url;
use uuid::Uuid;

use crate::{credential::CredentialStore, BrokerError, CredentialRef, CredentialVault};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_MAX_REDIRECTS: u8 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin(String);

impl Origin {
    pub fn parse(value: &str) -> Result<Self, BrokerError> {
        let url = Url::parse(value)
            .map_err(|_| BrokerError::InvalidRequest("origin is not a URL".to_owned()))?;
        Self::from_url(&url)
    }

    pub fn from_url(url: &Url) -> Result<Self, BrokerError> {
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(BrokerError::InvalidRequest(
                "only absolute HTTP(S) origins are allowed".to_owned(),
            ));
        }
        Ok(Self(url.origin().ascii_serialization()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn matches(&self, url: &Url) -> bool {
        self.0 == url.origin().ascii_serialization()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
}

impl HttpMethod {
    fn reqwest(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
            Self::Put => Method::PUT,
            Self::Patch => Method::PATCH,
            Self::Delete => Method::DELETE,
            Self::Head => Method::HEAD,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialPlacement {
    Bearer,
    Header(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialBinding {
    pub reference: CredentialRef,
    pub placement: CredentialPlacement,
    pub allowed_origins: Vec<Origin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyPolicy {
    Direct,
    ForwardOnly,
    DirectThenForward,
    ForwardThenDirect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    Direct,
    ForwardProxy,
}

#[derive(Debug, Clone)]
pub struct NetworkRequest {
    pub service: String,
    pub method: HttpMethod,
    pub url: Url,
    pub allowed_origins: Vec<Origin>,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Bytes>,
    pub credential: Option<CredentialBinding>,
    pub proxy_policy: ProxyPolicy,
    pub timeout: Duration,
    pub max_redirects: u8,
    pub max_response_bytes: usize,
    pub accept_http_errors: bool,
}

impl NetworkRequest {
    pub fn get(service: impl Into<String>, url: Url, allowed_origins: Vec<Origin>) -> Self {
        Self {
            service: service.into(),
            method: HttpMethod::Get,
            url,
            allowed_origins,
            headers: BTreeMap::new(),
            body: None,
            credential: None,
            proxy_policy: ProxyPolicy::Direct,
            timeout: DEFAULT_TIMEOUT,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            accept_http_errors: false,
        }
    }

    pub fn with_body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(body.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct ForwardProxy {
    endpoint: Url,
}

impl ForwardProxy {
    pub fn new(endpoint: Url) -> Result<Self, BrokerError> {
        Origin::from_url(&endpoint)?;
        Ok(Self { endpoint })
    }
}

#[derive(Debug, Clone)]
pub struct NetworkResponse {
    pub request_id: String,
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub route: RouteKind,
}

type ResponseByteStream =
    Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

pub struct NetworkStreamResponse {
    pub request_id: String,
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub route: RouteKind,
    stream: ResponseByteStream,
    endpoint: String,
    received_bytes: usize,
    max_response_bytes: usize,
    chunk_timeout: Duration,
}

impl NetworkStreamResponse {
    pub async fn next_chunk(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<Bytes>, BrokerError> {
        let outcome = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(BrokerError::Cancelled {
                    request_id: self.request_id.clone(),
                });
            }
            outcome = tokio::time::timeout(self.chunk_timeout, self.stream.next()) => outcome,
        };
        let Some(chunk) = outcome.map_err(|_| BrokerError::Timeout {
            request_id: self.request_id.clone(),
        })?
        else {
            return Ok(None);
        };
        let chunk = chunk.map_err(|_| BrokerError::Transport {
            request_id: self.request_id.clone(),
            endpoint: self.endpoint.clone(),
        })?;
        self.received_bytes = self
            .received_bytes
            .checked_add(chunk.len())
            .ok_or_else(|| BrokerError::ResponseTooLarge {
                request_id: self.request_id.clone(),
            })?;
        if self.received_bytes > self.max_response_bytes {
            return Err(BrokerError::ResponseTooLarge {
                request_id: self.request_id.clone(),
            });
        }
        Ok(Some(chunk))
    }
}

#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationToken {
    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::AcqRel) {
            self.inner.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

#[derive(Clone)]
pub struct NetworkBroker {
    client: Client,
    credentials: Arc<dyn CredentialStore>,
    forward_proxy: Option<ForwardProxy>,
}

impl NetworkBroker {
    pub fn new(
        credentials: &CredentialVault,
        forward_proxy: Option<ForwardProxy>,
    ) -> Result<Self, BrokerError> {
        let client = Client::builder()
            // Route selection is explicit; neither route may inherit system proxies.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| BrokerError::InvalidRequest("HTTP client setup failed".to_owned()))?;
        Ok(Self {
            client,
            credentials: credentials.resolver(),
            forward_proxy,
        })
    }

    pub async fn execute(
        &self,
        request: &NetworkRequest,
        cancellation: &CancellationToken,
    ) -> Result<NetworkResponse, BrokerError> {
        self.validate_request(request)?;
        let request_id = Uuid::new_v4().to_string();
        let routes = self.routes(request.proxy_policy)?;
        let mut last_transport_error = None;
        for route in routes {
            let future = self.execute_route(request, &request_id, route);
            let outcome = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    return Err(BrokerError::Cancelled { request_id });
                }
                outcome = tokio::time::timeout(request.timeout, future) => outcome,
            };
            match outcome {
                Err(_) => {
                    last_transport_error = Some(BrokerError::Timeout {
                        request_id: request_id.clone(),
                    });
                }
                Ok(Err(error @ BrokerError::Transport { .. })) => {
                    last_transport_error = Some(error);
                }
                Ok(result) => return result,
            }
        }
        Err(last_transport_error.unwrap_or(BrokerError::Transport {
            request_id,
            endpoint: redacted_endpoint(&request.url),
        }))
    }

    pub async fn open_stream(
        &self,
        request: &NetworkRequest,
        cancellation: &CancellationToken,
    ) -> Result<NetworkStreamResponse, BrokerError> {
        self.validate_request(request)?;
        let request_id = Uuid::new_v4().to_string();
        let routes = self.routes(request.proxy_policy)?;
        let mut last_transport_error = None;
        for route in routes {
            let future = self.open_stream_route(request, &request_id, route);
            let outcome = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    return Err(BrokerError::Cancelled { request_id });
                }
                outcome = tokio::time::timeout(request.timeout, future) => outcome,
            };
            match outcome {
                Err(_) => {
                    last_transport_error = Some(BrokerError::Timeout {
                        request_id: request_id.clone(),
                    });
                }
                Ok(Err(error @ BrokerError::Transport { .. })) => {
                    last_transport_error = Some(error);
                }
                Ok(result) => return result,
            }
        }
        Err(last_transport_error.unwrap_or(BrokerError::Transport {
            request_id,
            endpoint: redacted_endpoint(&request.url),
        }))
    }

    async fn open_stream_route(
        &self,
        request: &NetworkRequest,
        request_id: &str,
        route: RouteKind,
    ) -> Result<NetworkStreamResponse, BrokerError> {
        match route {
            RouteKind::Direct => self.open_stream_direct(request, request_id).await,
            RouteKind::ForwardProxy => self.open_stream_forward(request, request_id).await,
        }
    }

    async fn open_stream_direct(
        &self,
        request: &NetworkRequest,
        request_id: &str,
    ) -> Result<NetworkStreamResponse, BrokerError> {
        let credential = request
            .credential
            .as_ref()
            .map(|binding| {
                self.credentials
                    .resolve(&binding.reference)?
                    .ok_or_else(|| {
                        BrokerError::CredentialUnavailable(binding.reference.as_str().to_owned())
                    })
            })
            .transpose()?;
        let mut current = request.url.clone();
        for redirect_count in 0..=request.max_redirects {
            require_allowed_origin(&current, &request.allowed_origins)?;
            let headers = build_headers(
                &request.headers,
                request.credential.as_ref(),
                credential.as_ref(),
                &current,
            )?;
            let response = self
                .client
                .request(request.method.reqwest(), current.clone())
                .headers(headers)
                .body(request.body.clone().unwrap_or_default())
                .send()
                .await
                .map_err(|_| BrokerError::Transport {
                    request_id: request_id.to_owned(),
                    endpoint: redacted_endpoint(&current),
                })?;
            if response.status().is_redirection() {
                if redirect_count == request.max_redirects {
                    return Err(BrokerError::InvalidRedirect(
                        "redirect limit exceeded".to_owned(),
                    ));
                }
                current = redirect_target(&current, response.headers())?;
                require_allowed_origin(&current, &request.allowed_origins)?;
                continue;
            }
            return build_stream_response(
                response,
                request,
                request_id,
                &current,
                RouteKind::Direct,
            );
        }
        Err(BrokerError::InvalidRedirect(
            "redirect limit exceeded".to_owned(),
        ))
    }

    async fn open_stream_forward(
        &self,
        request: &NetworkRequest,
        request_id: &str,
    ) -> Result<NetworkStreamResponse, BrokerError> {
        if request.credential.is_some() {
            return Err(BrokerError::InvalidRequest(
                "credentialed requests cannot use a forwarding proxy".to_owned(),
            ));
        }
        let proxy = self.forward_proxy.as_ref().ok_or_else(|| {
            BrokerError::InvalidRequest("forward proxy is not configured".to_owned())
        })?;
        let mut endpoint = proxy.endpoint.clone();
        endpoint
            .query_pairs_mut()
            .append_pair("url", request.url.as_str());
        let headers = build_headers(&request.headers, None, None, &endpoint)?;
        let response = self
            .client
            .request(request.method.reqwest(), endpoint.clone())
            .headers(headers)
            .body(request.body.clone().unwrap_or_default())
            .send()
            .await
            .map_err(|_| BrokerError::Transport {
                request_id: request_id.to_owned(),
                endpoint: redacted_endpoint(&endpoint),
            })?;
        if response.status().is_redirection() {
            return Err(BrokerError::InvalidRedirect(
                "forward proxy responses cannot redirect".to_owned(),
            ));
        }
        build_stream_response(
            response,
            request,
            request_id,
            &endpoint,
            RouteKind::ForwardProxy,
        )
    }

    fn validate_request(&self, request: &NetworkRequest) -> Result<(), BrokerError> {
        if request.service.trim().is_empty() {
            return Err(BrokerError::InvalidRequest(
                "service label cannot be empty".to_owned(),
            ));
        }
        if request.allowed_origins.is_empty() {
            return Err(BrokerError::InvalidRequest(
                "at least one source origin is required".to_owned(),
            ));
        }
        require_allowed_origin(&request.url, &request.allowed_origins)?;
        if request.max_response_bytes == 0 {
            return Err(BrokerError::InvalidRequest(
                "response limit must be positive".to_owned(),
            ));
        }
        for name in request.headers.keys() {
            let normalized = name.to_ascii_lowercase();
            if matches!(
                normalized.as_str(),
                "authorization" | "proxy-authorization" | "cookie" | "x-api-key"
            ) {
                return Err(BrokerError::InvalidRequest(format!(
                    "sensitive header {name} must use a credential binding"
                )));
            }
        }
        if let Some(binding) = &request.credential {
            if binding.allowed_origins.is_empty()
                || !binding
                    .allowed_origins
                    .iter()
                    .any(|origin| origin.matches(&request.url))
            {
                return Err(BrokerError::CredentialUnavailable(
                    binding.reference.as_str().to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn routes(&self, policy: ProxyPolicy) -> Result<Vec<RouteKind>, BrokerError> {
        let has_proxy = self.forward_proxy.is_some();
        match policy {
            ProxyPolicy::Direct => Ok(vec![RouteKind::Direct]),
            ProxyPolicy::ForwardOnly if has_proxy => Ok(vec![RouteKind::ForwardProxy]),
            ProxyPolicy::DirectThenForward if has_proxy => {
                Ok(vec![RouteKind::Direct, RouteKind::ForwardProxy])
            }
            ProxyPolicy::ForwardThenDirect if has_proxy => {
                Ok(vec![RouteKind::ForwardProxy, RouteKind::Direct])
            }
            _ => Err(BrokerError::InvalidRequest(
                "request selected an unavailable forward proxy".to_owned(),
            )),
        }
    }

    async fn execute_route(
        &self,
        request: &NetworkRequest,
        request_id: &str,
        route: RouteKind,
    ) -> Result<NetworkResponse, BrokerError> {
        match route {
            RouteKind::Direct => self.execute_direct(request, request_id).await,
            RouteKind::ForwardProxy => self.execute_forward(request, request_id).await,
        }
    }

    async fn execute_direct(
        &self,
        request: &NetworkRequest,
        request_id: &str,
    ) -> Result<NetworkResponse, BrokerError> {
        let credential = request
            .credential
            .as_ref()
            .map(|binding| {
                self.credentials
                    .resolve(&binding.reference)?
                    .ok_or_else(|| {
                        BrokerError::CredentialUnavailable(binding.reference.as_str().to_owned())
                    })
            })
            .transpose()?;
        let mut current = request.url.clone();
        for redirect_count in 0..=request.max_redirects {
            require_allowed_origin(&current, &request.allowed_origins)?;
            let headers = build_headers(
                &request.headers,
                request.credential.as_ref(),
                credential.as_ref(),
                &current,
            )?;
            let response = self
                .client
                .request(request.method.reqwest(), current.clone())
                .headers(headers)
                .body(request.body.clone().unwrap_or_default())
                .send()
                .await
                .map_err(|_| BrokerError::Transport {
                    request_id: request_id.to_owned(),
                    endpoint: redacted_endpoint(&current),
                })?;
            if response.status().is_redirection() {
                if redirect_count == request.max_redirects {
                    return Err(BrokerError::InvalidRedirect(
                        "redirect limit exceeded".to_owned(),
                    ));
                }
                current = redirect_target(&current, response.headers())?;
                require_allowed_origin(&current, &request.allowed_origins)?;
                continue;
            }
            return collect_response(response, request, request_id, &current, RouteKind::Direct)
                .await;
        }
        Err(BrokerError::InvalidRedirect(
            "redirect limit exceeded".to_owned(),
        ))
    }

    async fn execute_forward(
        &self,
        request: &NetworkRequest,
        request_id: &str,
    ) -> Result<NetworkResponse, BrokerError> {
        if request.credential.is_some() {
            return Err(BrokerError::InvalidRequest(
                "credentialed requests cannot use a forwarding proxy".to_owned(),
            ));
        }
        let proxy = self.forward_proxy.as_ref().ok_or_else(|| {
            BrokerError::InvalidRequest("forward proxy is not configured".to_owned())
        })?;
        let mut endpoint = proxy.endpoint.clone();
        endpoint
            .query_pairs_mut()
            .append_pair("url", request.url.as_str());
        let headers = build_headers(&request.headers, None, None, &endpoint)?;
        let response = self
            .client
            .request(request.method.reqwest(), endpoint.clone())
            .headers(headers)
            .body(request.body.clone().unwrap_or_default())
            .send()
            .await
            .map_err(|_| BrokerError::Transport {
                request_id: request_id.to_owned(),
                endpoint: redacted_endpoint(&endpoint),
            })?;
        if response.status().is_redirection() {
            return Err(BrokerError::InvalidRedirect(
                "forward proxy responses cannot redirect".to_owned(),
            ));
        }
        collect_response(
            response,
            request,
            request_id,
            &endpoint,
            RouteKind::ForwardProxy,
        )
        .await
    }
}

fn require_allowed_origin(url: &Url, allowed: &[Origin]) -> Result<(), BrokerError> {
    allowed
        .iter()
        .any(|origin| origin.matches(url))
        .then_some(())
        .ok_or_else(|| BrokerError::DisallowedOrigin(url.origin().ascii_serialization()))
}

fn build_headers(
    configured: &BTreeMap<String, String>,
    binding: Option<&CredentialBinding>,
    credential: Option<&crate::SecretValue>,
    target: &Url,
) -> Result<HeaderMap, BrokerError> {
    let mut headers = HeaderMap::new();
    for (name, value) in configured {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| BrokerError::InvalidRequest("invalid header name".to_owned()))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| BrokerError::InvalidRequest("invalid header value".to_owned()))?;
        headers.insert(name, value);
    }
    let Some(binding) = binding else {
        return Ok(headers);
    };
    if !binding
        .allowed_origins
        .iter()
        .any(|origin| origin.matches(target))
    {
        return Ok(headers);
    }
    let credential = credential
        .ok_or_else(|| BrokerError::CredentialUnavailable(binding.reference.as_str().to_owned()))?;
    match &binding.placement {
        CredentialPlacement::Bearer => {
            let mut value = Vec::with_capacity(7 + credential.expose().len());
            value.extend_from_slice(b"Bearer ");
            value.extend_from_slice(credential.expose());
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_bytes(&value).map_err(|_| BrokerError::CredentialStore)?,
            );
            value.fill(0);
        }
        CredentialPlacement::Header(name) => {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                BrokerError::InvalidRequest("invalid credential header name".to_owned())
            })?;
            headers.insert(
                name,
                HeaderValue::from_bytes(credential.expose())
                    .map_err(|_| BrokerError::CredentialStore)?,
            );
        }
    }
    Ok(headers)
}

fn redirect_target(current: &Url, headers: &HeaderMap) -> Result<Url, BrokerError> {
    let location = headers
        .get(LOCATION)
        .ok_or_else(|| BrokerError::InvalidRedirect("Location is missing".to_owned()))?
        .to_str()
        .map_err(|_| BrokerError::InvalidRedirect("Location is not UTF-8".to_owned()))?;
    let target = current
        .join(location)
        .map_err(|_| BrokerError::InvalidRedirect("Location is not a URL".to_owned()))?;
    Origin::from_url(&target)?;
    Ok(target)
}

fn build_stream_response(
    response: reqwest::Response,
    request: &NetworkRequest,
    request_id: &str,
    endpoint: &Url,
    route: RouteKind,
) -> Result<NetworkStreamResponse, BrokerError> {
    let status = response.status();
    if !request.accept_http_errors && !status.is_success() {
        return Err(BrokerError::Http {
            request_id: request_id.to_owned(),
            status: status.as_u16(),
            endpoint: redacted_endpoint(endpoint),
        });
    }
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > request.max_response_bytes)
    {
        return Err(BrokerError::ResponseTooLarge {
            request_id: request_id.to_owned(),
        });
    }
    let headers = safe_response_headers(response.headers());
    Ok(NetworkStreamResponse {
        request_id: request_id.to_owned(),
        status: status.as_u16(),
        headers,
        route,
        stream: Box::pin(response.bytes_stream()),
        endpoint: redacted_endpoint(endpoint),
        received_bytes: 0,
        max_response_bytes: request.max_response_bytes,
        chunk_timeout: request.timeout,
    })
}

async fn collect_response(
    response: reqwest::Response,
    request: &NetworkRequest,
    request_id: &str,
    endpoint: &Url,
    route: RouteKind,
) -> Result<NetworkResponse, BrokerError> {
    let status = response.status();
    if !request.accept_http_errors && !status.is_success() {
        return Err(BrokerError::Http {
            request_id: request_id.to_owned(),
            status: status.as_u16(),
            endpoint: redacted_endpoint(endpoint),
        });
    }
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > request.max_response_bytes)
    {
        return Err(BrokerError::ResponseTooLarge {
            request_id: request_id.to_owned(),
        });
    }
    let headers = safe_response_headers(response.headers());
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| BrokerError::Transport {
            request_id: request_id.to_owned(),
            endpoint: redacted_endpoint(endpoint),
        })?;
        let next_len =
            body.len()
                .checked_add(chunk.len())
                .ok_or_else(|| BrokerError::ResponseTooLarge {
                    request_id: request_id.to_owned(),
                })?;
        if next_len > request.max_response_bytes {
            return Err(BrokerError::ResponseTooLarge {
                request_id: request_id.to_owned(),
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(NetworkResponse {
        request_id: request_id.to_owned(),
        status: status.as_u16(),
        headers,
        body,
        route,
    })
}

fn safe_response_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str();
            matches!(
                name,
                "content-type"
                    | "content-length"
                    | "content-range"
                    | "content-md5"
                    | "accept-ranges"
                    | "etag"
                    | "last-modified"
                    | "retry-after"
            )
            .then(|| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.to_owned(), value.to_owned()))
            })
            .flatten()
        })
        .collect()
}

fn redacted_endpoint(url: &Url) -> String {
    let mut redacted = url.clone();
    let _ = redacted.set_username("");
    let _ = redacted.set_password(None);
    redacted.set_query(None);
    redacted.set_fragment(None);
    redacted.to_string()
}
