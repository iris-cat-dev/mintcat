use mintcat_game_api::{GameDescriptor, GameInputFormat, GameVariantDescriptor};
use mintcat_sdk::{run_worker, worker_app_error, WorkerIdentity};
use serde_json::{json, Value};

pub fn descriptor(game_id: &str, version: &str) -> GameDescriptor {
    GameDescriptor {
        schema_version: 1, game_id: game_id.into(), display_name: format!("TEST ONLY {game_id}"),
        adapter_version: version.into(), settings_schema_version: 1,
        settings_schema: json!({"type":"object"}),
        variants: vec![GameVariantDescriptor { variant_id: "fixture".into(), display_name: "Fixture".into(),
            store: "fixture".into(), store_app_id: None, target: "x86_64-pc-windows-msvc".into(),
            main_pak_relative_path: "Content/Paks/base.pak".into(), paks_relative_path: "Content/Paks".into(),
            binaries_relative_path: "Binaries/Win64".into(), mod_pak_relative_path: "Content/Paks/mods.pak".into(),
            process_names: vec!["FixtureGame.exe".into()] }],
        input_formats: vec![GameInputFormat::Pak], source_bindings: vec![], resources: vec![],
        capabilities: vec!["describe".into()],
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let identity = WorkerIdentity::from_env()?;
    let game_id = identity.module_id.strip_prefix("fixture.").ok_or("fixture module identity required")?.to_owned();
    let descriptor = descriptor(&game_id, &identity.module_version);
    run_worker(identity, move |request| {
        let descriptor = descriptor.clone();
        async move {
            match request.method.as_deref() {
                Some("describe") => Ok(serde_json::to_value(descriptor).expect("fixture descriptor JSON")),
                Some("math.sum") => Ok(json!({"sum": request.params["values"].as_array().expect("fixture values").iter().map(|value| value.as_f64().expect("fixture number")).sum::<f64>()})),
                _ => Err(worker_app_error(&request.id, "UNKNOWN_METHOD", "fixture.unknown", Value::Null)),
            }
        }
    }).await?;
    Ok(())
}
