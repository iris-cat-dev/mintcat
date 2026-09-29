use serde::Deserialize;
use tauri::AppHandle;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlPlaneInvokeRequest {
    pub command: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[tauri::command]
pub async fn control_plane_invoke(
    app: AppHandle,
    request: ControlPlaneInvokeRequest,
) -> Result<serde_json::Value, String> {
    match request.command.as_str() {
        "open_managed_webview" => {
            let payload = serde_json::from_value(request.payload)
                .map_err(|error| format!("invalid open_managed_webview payload: {error}"))?;
            crate::webview_host::open_managed_webview(app, payload).await?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "agent_llm_set_api_key" => {
            let api_key = request
                .payload
                .get("apiKey")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "invalid agent_llm_set_api_key payload".to_string())?;
            crate::agent::set_api_key(api_key.to_owned()).await
        }
        "agent_llm_has_api_key" => crate::agent::has_api_key().await,
        "agent_llm_clear_api_key" => crate::agent::set_api_key(String::new()).await,
        "agent_llm_chat_completion" => {
            let payload = serde_json::from_value(request.payload)
                .map_err(|error| format!("invalid agent_llm_chat_completion payload: {error}"))?;
            crate::agent::chat_completion(&app, payload).await
        }
        command => Err(format!("unknown control plane command: {command}")),
    }
}
