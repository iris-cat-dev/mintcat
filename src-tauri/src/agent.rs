use crate::network;
use keyring::Entry;
use reqwest::Url;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    error::Error as StdError,
    sync::{LazyLock, RwLock},
    time::Duration,
};
use tauri::{AppHandle, Manager};

const KEYRING_SERVICE: &str = "com.mint.cat.agent";
const KEYRING_ACCOUNT: &str = "openai-compatible";
const REQUEST_TIMEOUT_SECS: u64 = 120;
const CONNECT_TIMEOUT_SECS: u64 = 15;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
static API_KEY_CACHE: LazyLock<RwLock<Option<String>>> = LazyLock::new(|| RwLock::new(None));

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentChatCompletionRequest {
    pub base_url: String,
    pub body: Value,
    #[serde(default)]
    pub api_key: Option<String>,
}

fn credential_entry() -> Result<Entry, String> {
    Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
        .map_err(|error| format!("cannot open system credential store: {error}"))
}

fn cached_api_key() -> Result<Option<String>, String> {
    API_KEY_CACHE
        .read()
        .map(|value| value.clone())
        .map_err(|_| "API key cache lock is poisoned".to_string())
}

fn set_cached_api_key(api_key: Option<String>) -> Result<(), String> {
    *API_KEY_CACHE
        .write()
        .map_err(|_| "API key cache lock is poisoned".to_string())? = api_key;
    Ok(())
}

pub async fn set_api_key(api_key: String) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        let entry = credential_entry()?;
        let api_key = api_key.trim().to_owned();
        if api_key.is_empty() {
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {
                    set_cached_api_key(None)?;
                    Ok(json!({"ok": true}))
                }
                Err(error) => Err(format!("cannot clear API key: {error}")),
            }
        } else {
            entry
                .set_password(&api_key)
                .map_err(|error| format!("cannot save API key: {error}"))?;
            set_cached_api_key(Some(api_key))?;
            Ok(json!({"ok": true}))
        }
    })
    .await
    .map_err(|error| format!("credential task failed: {error}"))?
}

pub async fn has_api_key() -> Result<Value, String> {
    if cached_api_key()?.is_some() {
        return Ok(json!({"configured": true}));
    }
    tokio::task::spawn_blocking(move || {
        let entry = credential_entry()?;
        match entry.get_password() {
            Ok(value) => {
                let configured = !value.trim().is_empty();
                if configured {
                    set_cached_api_key(Some(value))?;
                }
                Ok(json!({"configured": configured}))
            }
            Err(keyring::Error::NoEntry) => Ok(json!({"configured": false})),
            Err(error) => Err(format!("cannot read API key status: {error}")),
        }
    })
    .await
    .map_err(|error| format!("credential task failed: {error}"))?
}

fn normalize_chat_completions_url(base_url: &str) -> Result<Url, String> {
    let mut url =
        Url::parse(base_url.trim()).map_err(|error| format!("invalid LLM base URL: {error}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("LLM base URL must use http or https".to_string());
    }
    if url.host_str().is_none() || url.query().is_some() || url.fragment().is_some() {
        return Err("LLM base URL must have a host and no query or fragment".to_string());
    }

    let path = url.path().trim_end_matches('/');
    if !path.ends_with("/chat/completions") {
        let next_path = if path.is_empty() {
            "/v1/chat/completions".to_string()
        } else {
            format!("{path}/chat/completions")
        };
        url.set_path(&next_path);
    }
    Ok(url)
}

fn response_error(status: reqwest::StatusCode, body: &[u8]) -> String {
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(500).collect());
    format!("LLM request failed ({status}): {message}")
}

fn transport_error(error: &reqwest::Error, uses_proxy: bool) -> String {
    let kind = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connection"
    } else {
        "transport"
    };
    let route = if uses_proxy { "proxy" } else { "direct" };
    let mut causes = Vec::new();
    let mut source = error.source();
    while let Some(cause) = source {
        let message = cause.to_string();
        if causes.last() != Some(&message) {
            causes.push(message);
        }
        source = cause.source();
    }
    let detail = if causes.is_empty() {
        error.to_string()
    } else {
        causes.join(": ")
    };
    format!("LLM request failed before receiving a response ({kind}, route: {route}): {detail}")
}

fn authorize_request(
    request: reqwest::RequestBuilder,
    api_key: Option<String>,
) -> reqwest::RequestBuilder {
    match api_key.filter(|value| !value.trim().is_empty()) {
        Some(key) => request.bearer_auth(key),
        None => request,
    }
}

pub async fn chat_completion(
    app: &AppHandle,
    request: AgentChatCompletionRequest,
) -> Result<Value, String> {
    let url = normalize_chat_completions_url(&request.base_url)?;
    let request_api_key = request
        .api_key
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_owned());
    let api_key = if request_api_key.is_some() {
        request_api_key
    } else if let Some(value) = cached_api_key()? {
        Some(value)
    } else {
        tokio::task::spawn_blocking(move || match credential_entry()?.get_password() {
            Ok(value) => {
                let value = value.trim().to_owned();
                if value.is_empty() {
                    Ok(None)
                } else {
                    set_cached_api_key(Some(value.clone()))?;
                    Ok(Some(value))
                }
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(format!("cannot read API key: {error}")),
        })
        .await
        .map_err(|error| format!("credential task failed: {error}"))??
    };

    let manual_proxy = app.state::<network::NetworkProxyState>().get();
    let proxy_url = network::resolve_proxy(manual_proxy);
    let uses_proxy = proxy_url.is_some();
    let builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS));
    let client = network::apply_proxy_builder(builder, proxy_url.as_deref())
        .map_err(|error| format!("cannot configure LLM HTTP client: {error}"))?
        .build()
        .map_err(|error| format!("cannot build LLM HTTP client: {error}"))?;

    let http_request = authorize_request(client.post(url).json(&request.body), api_key);
    let response = http_request
        .send()
        .await
        .map_err(|error| transport_error(&error, uses_proxy))?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err("LLM response exceeded 8 MiB".to_string());
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("cannot read LLM response: {error}"))?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err("LLM response exceeded 8 MiB".to_string());
    }
    if !status.is_success() {
        return Err(response_error(status, &body));
    }
    serde_json::from_slice(&body).map_err(|error| format!("invalid LLM JSON response: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        authorize_request, cached_api_key, normalize_chat_completions_url, set_cached_api_key,
        AgentChatCompletionRequest,
    };

    #[test]
    fn request_key_builds_bearer_authorization() {
        let payload = serde_json::json!({
            "baseUrl": "https://example.com/v1",
            "body": {},
            "apiKey": "direct-key"
        });
        let completion: AgentChatCompletionRequest = serde_json::from_value(payload).unwrap();
        let request = authorize_request(
            reqwest::Client::new().post("https://example.com/v1/chat/completions"),
            completion.api_key,
        )
        .build()
        .unwrap();

        assert_eq!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .unwrap(),
            "Bearer direct-key"
        );
    }

    #[test]
    fn cached_key_builds_bearer_authorization() {
        set_cached_api_key(Some("test-key".to_string())).unwrap();
        let request = authorize_request(
            reqwest::Client::new().post("https://example.com/v1/chat/completions"),
            cached_api_key().unwrap(),
        )
        .build()
        .unwrap();
        set_cached_api_key(None).unwrap();

        assert_eq!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .unwrap(),
            "Bearer test-key"
        );
    }

    #[test]
    fn appends_chat_completions_path() {
        assert_eq!(
            normalize_chat_completions_url("https://api.openai.com/v1")
                .unwrap()
                .as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn preserves_full_endpoint() {
        assert_eq!(
            normalize_chat_completions_url("http://localhost:11434/v1/chat/completions")
                .unwrap()
                .as_str(),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn rejects_non_http_and_query_urls() {
        assert!(normalize_chat_completions_url("file:///tmp/model").is_err());
        assert!(normalize_chat_completions_url("https://example.com/v1?token=secret").is_err());
    }
}
