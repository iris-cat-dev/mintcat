use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::Path;

const WATT_API_BASE: &str = "https://api.steampp.net";
const CACHE_FILENAME: &str = "accelerate_cache.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProxyType {
    Local = 0,
    Redirect = 1,
    ServerAccelerate = 2,
    ServerProxy = 4,
}

impl Default for ProxyType {
    fn default() -> Self {
        Self::Local
    }
}

impl<'de> Deserialize<'de> for ProxyType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ProxyType;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("integer 0-2 or string proxy type")
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<ProxyType, E> {
                match v {
                    0 => Ok(ProxyType::Local),
                    1 => Ok(ProxyType::Redirect),
                    2 => Ok(ProxyType::ServerAccelerate),
                    4 => Ok(ProxyType::ServerProxy),
                    _ => Ok(ProxyType::Local),
                }
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<ProxyType, E> {
                self.visit_u64(v as u64)
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<ProxyType, E> {
                match v {
                    "0" | "local" | "Local" => Ok(ProxyType::Local),
                    "1" | "redirect" | "Redirect" => Ok(ProxyType::Redirect),
                    "2" | "serverAccelerate" | "ServerAccelerate" => {
                        Ok(ProxyType::ServerAccelerate)
                    }
                    "4" | "serverProxy" | "ServerProxy" => Ok(ProxyType::ServerProxy),
                    _ => Ok(ProxyType::Local),
                }
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccelerateProject {
    #[serde(default, alias = "0")]
    pub name: String,
    #[serde(default, alias = "1")]
    pub port: u16,
    #[serde(default, alias = "2")]
    pub match_domain_names: String,
    #[serde(default, alias = "3")]
    pub forward_domain_names: String,
    #[serde(default, alias = "7")]
    pub listen_domain_names: String,
    #[serde(default, alias = "ProxyType")]
    pub proxy_type: ProxyType,
    #[serde(default, alias = "5")]
    pub fake_server_name: Option<String>,
    #[serde(default)]
    pub ignore_ssl_cert_verification: bool,
    #[serde(default, alias = "12")]
    pub items: Option<Vec<AccelerateProject>>,
    #[serde(default, alias = "10")]
    pub order: i64,
    #[serde(default, alias = "8")]
    pub checked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccelerateProjectGroup {
    #[serde(default, alias = "0")]
    pub name: String,
    #[serde(default, alias = "4")]
    pub order: i64,
    #[serde(default, alias = "1")]
    pub items: Option<Vec<AccelerateProject>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    #[serde(alias = "isSuccess")]
    pub is_success: Option<bool>,
    #[serde(alias = "code", alias = "\u{1F984}")]
    pub code: Option<i32>,
    #[serde(alias = "\u{1F993}")]
    pub content: Option<T>,
}

#[derive(Debug, Clone)]
pub struct DomainConfig {
    pub name: String,
    pub match_patterns: Vec<String>,
    pub listen_domains: Vec<String>,
    pub forward_ip: Option<IpAddr>,
    pub forward_domain: Option<String>,
    pub destination_url: Option<String>,
    pub is_server_proxy: bool,
    #[allow(dead_code)]
    pub fake_sni: Option<String>,
    #[allow(dead_code)]
    pub ignore_cert: bool,
    pub port: u16,
    pub proxy_url: Option<String>,
}

pub struct WattApiClient {
    client: reqwest::Client,
}

impl WattApiClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build HTTP client");
        Self { client }
    }

    pub async fn fetch_all(&self) -> Result<Vec<AccelerateProjectGroup>> {
        let url = format!("{}/api/Accelerate/All", WATT_API_BASE);
        tracing::info!("fetching accelerate rules from {}", url);

        let resp = self
            .client
            .get(&url)
            .header("User-Agent", "mintcat-proxy/0.1")
            .send()
            .await
            .context("failed to reach Watt API")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Watt API returned HTTP {}: {}", status, body);
        }

        let api_resp: ApiResponse<Vec<AccelerateProjectGroup>> = resp
            .json()
            .await
            .context("failed to parse Watt API response")?;

        api_resp.content.context("Watt API returned empty content")
    }

    pub fn save_cache(data_dir: &Path, groups: &[AccelerateProjectGroup]) -> Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join(CACHE_FILENAME);
        let json = serde_json::to_string_pretty(groups)?;
        std::fs::write(&path, json)?;
        tracing::info!("saved accelerate cache to {}", path.display());
        Ok(())
    }

    pub fn load_cache(data_dir: &Path) -> Result<Vec<AccelerateProjectGroup>> {
        let path = data_dir.join(CACHE_FILENAME);
        let json = std::fs::read_to_string(&path)
            .with_context(|| format!("no cache at {}", path.display()))?;
        let groups: Vec<AccelerateProjectGroup> = serde_json::from_str(&json)?;
        tracing::info!("loaded {} groups from cache", groups.len());
        Ok(groups)
    }
}


const CUSTOM_RULES_FILENAME: &str = "custom_rules.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomRule {
    pub name: String,
    pub domains: Vec<String>,
    #[serde(default)]
    pub forward: Option<String>,
    #[serde(default)]
    pub proxy: Option<String>,
}

pub fn load_custom_rules(data_dir: &Path) -> Vec<DomainConfig> {
    let path = data_dir.join(CUSTOM_RULES_FILENAME);
    if !path.exists() {
        return Vec::new();
    }

    let json = match std::fs::read_to_string(&path) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("failed to read {}: {}", path.display(), e);
            return Vec::new();
        }
    };

    let rules: Vec<CustomRule> = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("failed to parse {}: {}", path.display(), e);
            return Vec::new();
        }
    };

    tracing::info!(
        "loaded {} custom rules from {}",
        rules.len(),
        path.display()
    );

    rules
        .into_iter()
        .map(|r| {
            let forward = r.forward.as_deref().unwrap_or("").trim();
            let mut forward_ip = None;
            let mut forward_domain = None;
            let mut destination_url = None;
            let mut is_server_proxy = false;

            if forward.starts_with("http://") || forward.starts_with("https://") {
                destination_url = Some(forward.to_string());
                is_server_proxy = true;
            } else if let Ok(ip) = forward.parse::<IpAddr>() {
                forward_ip = Some(ip);
            } else if !forward.is_empty() {
                forward_domain = Some(forward.to_string());
            }

            let proxy_url = r
                .proxy
                .map(|p| p.trim_end_matches('/').to_string())
                .filter(|p| !p.is_empty());

            DomainConfig {
                name: r.name.clone(),
                match_patterns: r.domains.clone(),
                listen_domains: r.domains,
                forward_ip,
                forward_domain,
                destination_url,
                is_server_proxy,
                fake_sni: None,
                ignore_cert: false,
                port: 443,
                proxy_url,
            }
        })
        .collect()
}


fn split_semicolon(s: &str) -> Vec<String> {
    s.split(';')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn split_domain_list(s: &str) -> Vec<String> {
    s.split(|c: char| c == ';' || c == '\n')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

pub fn flatten_projects(groups: &[AccelerateProjectGroup]) -> Vec<DomainConfig> {
    let mut configs = Vec::new();
    for group in groups {
        if let Some(items) = &group.items {
            for item in items {
                collect_domain_configs(item, &mut configs);
            }
        }
    }
    configs
}

fn collect_domain_configs(project: &AccelerateProject, out: &mut Vec<DomainConfig>) {
    if let Some(children) = &project.items {
        for child in children {
            collect_domain_configs(child, out);
        }
        if project.match_domain_names.is_empty() {
            return;
        }
    }

    let match_patterns = split_semicolon(&project.match_domain_names);
    let listen_domains = split_domain_list(&project.listen_domain_names);

    if match_patterns.is_empty() && listen_domains.is_empty() {
        return;
    }

    let forward = project.forward_domain_names.trim();
    let mut forward_ip: Option<IpAddr> = None;
    let mut forward_domain: Option<String> = None;
    let mut destination_url: Option<String> = None;
    let is_server_proxy;

    match project.proxy_type {
        ProxyType::Local => {
            is_server_proxy = false;
            if let Ok(ip) = forward.parse::<IpAddr>() {
                forward_ip = Some(ip);
            } else if !forward.is_empty() {
                forward_domain = Some(forward.to_string());
            }
        }
        ProxyType::Redirect => {
            is_server_proxy = false;
            if !forward.is_empty() {
                let scheme = if project.port == 443 { "https" } else { "http" };
                destination_url = Some(format!("{}://{}:{}", scheme, forward, project.port));
            }
        }
        ProxyType::ServerAccelerate | ProxyType::ServerProxy => {
            is_server_proxy = true;
            if !forward.is_empty() {
                destination_url = Some(forward.to_string());
            }
        }
    }

    out.push(DomainConfig {
        name: project.name.clone(),
        match_patterns,
        listen_domains,
        forward_ip,
        forward_domain,
        destination_url,
        is_server_proxy,
        fake_sni: project.fake_server_name.clone().filter(|s| !s.is_empty()),
        ignore_cert: project.ignore_ssl_cert_verification,
        port: project.port,
        proxy_url: None,
    });
}

pub fn find_config_for_host<'a>(
    configs: &'a [DomainConfig],
    host: &str,
) -> Option<&'a DomainConfig> {
    let host_lower = host.to_lowercase();
    configs.iter().find(|c| {
        c.match_patterns
            .iter()
            .any(|p| domain_matches(p, &host_lower))
            || c.listen_domains
                .iter()
                .any(|d| d.eq_ignore_ascii_case(host))
    })
}

fn domain_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.to_lowercase();
    if pattern.starts_with("*.") {
        let suffix = &pattern[1..]; // ".example.com"
        host.ends_with(suffix) || host == &pattern[2..]
    } else if pattern.starts_with('/') {
        // regex pattern — simplified: just check contains
        host.contains(pattern.trim_start_matches("/^").trim_end_matches("$/"))
    } else {
        host == pattern
    }
}
