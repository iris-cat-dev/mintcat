use crate::config::{find_config_for_host, DomainConfig};
use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use std::sync::Arc;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, hyper::Error>;

fn full_body(data: impl Into<Bytes>) -> BoxBody {
    Full::new(data.into())
        .map_err(|never| match never {})
        .boxed()
}

pub async fn handle_request(
    req: Request<Incoming>,
    configs: Arc<Vec<DomainConfig>>,
    http_client: reqwest::Client,
) -> Result<Response<BoxBody>> {
    let host = extract_host(&req);
    tracing::info!("{} {} (host: {})", req.method(), req.uri(), host);

    let config = find_config_for_host(&configs, &host);

    match &config {
        Some(c) => tracing::info!(
            "matched rule [{}]: proxy={:?}, fwd_ip={:?}, fwd_domain={:?}, dest_url={:?}",
            c.name,
            c.proxy_url,
            c.forward_ip,
            c.forward_domain,
            c.destination_url,
        ),
        None => tracing::warn!("no matching rule for host: {}", host),
    }

    let result = match config {
        Some(c) if c.proxy_url.is_some() => {
            let proxy = c.proxy_url.as_ref().unwrap().clone();
            forward_via_proxy(req, &proxy, &host, &http_client).await
        }
        Some(c) if c.forward_ip.is_some() => {
            forward_to_ip(
                req,
                c.forward_ip.unwrap().to_string(),
                &host,
                c,
                &http_client,
            )
            .await
        }
        Some(c) if c.forward_domain.is_some() => {
            let fwd = c.forward_domain.as_ref().unwrap().clone();
            forward_to_domain(req, &fwd, &host, c, &http_client).await
        }
        Some(c) if c.destination_url.is_some() => {
            let dest = c.destination_url.as_ref().unwrap().clone();
            forward_to_url(req, &dest, c.is_server_proxy, &http_client).await
        }
        _ => forward_passthrough(req, &host, &http_client).await,
    };

    result.or_else(|e| {
        tracing::error!("proxy error: {}", e);
        Ok(error_response(
            StatusCode::BAD_GATEWAY,
            &format!("Proxy error: {}", e),
        ))
    })
}

/// Forward IP case: URL uses the raw IP; SNI won't match but cert check is disabled.
async fn forward_to_ip(
    req: Request<Incoming>,
    ip: String,
    original_host: &str,
    config: &DomainConfig,
    client: &reqwest::Client,
) -> Result<Response<BoxBody>> {
    let pq = path_and_query(&req);
    let port = config.port;
    let upstream_url = if port == 443 || port == 0 {
        format!("https://{}{}", ip, pq)
    } else {
        format!("https://{}:{}{}", ip, port, pq)
    };
    do_forward(req, &upstream_url, original_host, false, client).await
}

/// Forward domain case: URL uses the forward domain (correct SNI), Host = original.
/// reqwest resolves the forward domain via the built-in DoH resolver.
async fn forward_to_domain(
    req: Request<Incoming>,
    forward_domain: &str,
    original_host: &str,
    config: &DomainConfig,
    client: &reqwest::Client,
) -> Result<Response<BoxBody>> {
    let pq = path_and_query(&req);
    let port = config.port;
    let upstream_url = if port == 443 || port == 0 {
        format!("https://{}{}", forward_domain, pq)
    } else {
        format!("https://{}:{}{}", forward_domain, port, pq)
    };
    do_forward(req, &upstream_url, original_host, false, client).await
}

/// DoH passthrough: URL uses the original hostname (correct SNI + Host).
/// reqwest resolves via DoH, bypassing the local hosts file.
async fn forward_passthrough(
    req: Request<Incoming>,
    host: &str,
    client: &reqwest::Client,
) -> Result<Response<BoxBody>> {
    let pq = path_and_query(&req);
    let upstream_url = format!("https://{}{}", host, pq);
    do_forward(req, &upstream_url, host, false, client).await
}

/// CF Worker proxy: URL-encode the target and send to `{proxy_base}/{encoded_target}`.
/// Host header is left as the Worker's domain (reqwest derives it from the URL).
async fn forward_via_proxy(
    req: Request<Incoming>,
    proxy_base: &str,
    host: &str,
    client: &reqwest::Client,
) -> Result<Response<BoxBody>> {
    let pq = path_and_query(&req).to_string();
    let method: reqwest::Method = req.method().clone().as_str().parse()?;

    let mut headers = Vec::new();
    for (name, value) in req.headers() {
        let n = name.as_str();
        if n != "host" && n != "connection" && n != "transfer-encoding" && n != "content-length" {
            if let Ok(v) = value.to_str() {
                headers.push((n.to_string(), v.to_string()));
            }
        }
    }

    let body_bytes = req.into_body().collect().await?.to_bytes();

    let target = format!("https://{}{}", host, pq);
    let upstream_url = format!("{}/{}", proxy_base, url_encode_path(&target));
    tracing::info!("proxying via CF worker: {} -> {}", host, upstream_url);

    let mut builder = client.request(method, &upstream_url);
    for (name, value) in &headers {
        builder = builder.header(name.as_str(), value.as_str());
    }

    let upstream_resp = match builder.body(body_bytes).send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(
                "CF worker request failed: url={}, err={:?}",
                upstream_url,
                e
            );
            return Err(e.into());
        }
    };

    let status = StatusCode::from_u16(upstream_resp.status().as_u16())?;
    let mut resp_builder = Response::builder().status(status);
    for (key, value) in upstream_resp.headers() {
        if key != "transfer-encoding" && key != "content-length" {
            resp_builder = resp_builder.header(key.as_str(), value.as_bytes());
        }
    }
    let resp_bytes = upstream_resp.bytes().await?;
    Ok(resp_builder.body(full_body(resp_bytes))?)
}

fn url_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(char::from(b"0123456789ABCDEF"[(b >> 4) as usize]));
                out.push(char::from(b"0123456789ABCDEF"[(b & 0xf) as usize]));
            }
        }
    }
    out
}

async fn forward_to_url(
    req: Request<Incoming>,
    dest_url: &str,
    is_server_proxy: bool,
    client: &reqwest::Client,
) -> Result<Response<BoxBody>> {
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let host = extract_host(&req);
    let upstream_url = if dest_url.ends_with('/') {
        format!("{}{}", dest_url.trim_end_matches('/'), path_and_query)
    } else {
        format!("{}{}", dest_url, path_and_query)
    };

    do_forward(req, &upstream_url, &host, is_server_proxy, client).await
}

async fn do_forward(
    req: Request<Incoming>,
    upstream_url: &str,
    original_host: &str,
    is_server_proxy: bool,
    client: &reqwest::Client,
) -> Result<Response<BoxBody>> {
    let method: reqwest::Method = req.method().clone().as_str().parse()?;

    let body_bytes = req.into_body().collect().await?.to_bytes();

    let mut builder = client
        .request(method, upstream_url)
        .header("Host", original_host);

    if is_server_proxy {
        builder = builder.header("X-Watt-Origin-Dest-Host", original_host);
    }

    tracing::debug!("forwarding {} -> {}", original_host, upstream_url);

    let upstream_resp = match builder.body(body_bytes).send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(
                "upstream request failed: url={}, host={}, is_connect={}, is_timeout={}, is_request={}, detail={}",
                upstream_url,
                original_host,
                e.is_connect(),
                e.is_timeout(),
                e.is_request(),
                e,
            );
            return Err(e.into());
        }
    };

    let status = StatusCode::from_u16(upstream_resp.status().as_u16())?;

    let mut resp_builder = Response::builder().status(status);
    for (key, value) in upstream_resp.headers() {
        if key != "transfer-encoding" && key != "content-length" {
            resp_builder = resp_builder.header(key.as_str(), value.as_bytes());
        }
    }

    let resp_bytes = upstream_resp.bytes().await?;
    let body = full_body(resp_bytes);

    Ok(resp_builder.body(body)?)
}

fn path_and_query(req: &Request<Incoming>) -> &str {
    req.uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
}

fn extract_host(req: &Request<Incoming>) -> String {
    if let Some(host) = req.headers().get("host") {
        if let Ok(h) = host.to_str() {
            return h.split(':').next().unwrap_or(h).to_string();
        }
    }
    if let Some(host) = req.uri().host() {
        return host.to_string();
    }
    "unknown".to_string()
}

fn error_response(status: StatusCode, msg: &str) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/plain; charset=utf-8")
        .body(full_body(msg.to_string()))
        .unwrap()
}
