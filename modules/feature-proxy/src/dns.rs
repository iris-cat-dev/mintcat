use anyhow::{Context, Result};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

pub struct DohResolver {
    client: reqwest::Client,
    providers: Vec<String>,
}

impl DohResolver {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .no_proxy()
            .build()
            .expect("failed to build DoH client");

        Self {
            client,
            providers: vec![
                "https://dns.alidns.com/resolve".into(),
                "https://doh.pub/resolve".into(),
            ],
        }
    }

    pub async fn resolve(&self, domain: &str) -> Result<IpAddr> {
        // Try each provider until one succeeds
        let mut last_err = None;
        for provider in &self.providers {
            match self.query_json(provider, domain).await {
                Ok(ip) => return Ok(ip),
                Err(e) => {
                    tracing::debug!("DoH provider {} failed for {}: {}", provider, domain, e);
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no DoH providers configured")))
    }

    async fn query_json(&self, provider: &str, domain: &str) -> Result<IpAddr> {
        let url = format!("{}?name={}&type=A", provider, domain);
        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/dns-json")
            .send()
            .await
            .with_context(|| format!("DoH request to {} failed", provider))?;

        let body: DnsJsonResponse = resp.json().await.context("failed to parse DoH JSON")?;

        if body.status != 0 {
            anyhow::bail!("DNS query returned status {}", body.status);
        }

        for answer in body.answer.unwrap_or_default() {
            // type 1 = A record, type 28 = AAAA record
            if answer.r#type == 1 || answer.r#type == 28 {
                if let Ok(ip) = answer.data.parse::<IpAddr>() {
                    return Ok(ip);
                }
            }
        }

        anyhow::bail!("no A/AAAA records found for {}", domain)
    }
}

/// Adapter that lets reqwest use DoH for all DNS lookups, bypassing the system hosts file.
pub struct ReqwestDohResolver(pub Arc<DohResolver>);

impl reqwest::dns::Resolve for ReqwestDohResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let doh = self.0.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            tracing::debug!("DoH resolving: {}", host);
            match doh.resolve(&host).await {
                Ok(ip) => {
                    tracing::info!("DoH resolved {} -> {}", host, ip);
                    let addr = SocketAddr::new(ip, 0);
                    Ok(Box::new(std::iter::once(addr)) as reqwest::dns::Addrs)
                }
                Err(e) => {
                    tracing::error!("DoH resolution failed for {}: {}", host, e);
                    Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        e.to_string(),
                    ))
                        as Box<dyn std::error::Error + Send + Sync>)
                }
            }
        })
    }
}

#[derive(serde::Deserialize)]
struct DnsJsonResponse {
    #[serde(alias = "Status")]
    status: i32,
    #[serde(alias = "Answer")]
    answer: Option<Vec<DnsAnswer>>,
}

#[derive(serde::Deserialize)]
struct DnsAnswer {
    #[serde(alias = "type", alias = "Type")]
    r#type: u16,
    #[serde(alias = "data")]
    data: String,
}
