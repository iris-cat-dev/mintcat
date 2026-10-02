use super::*;
use mintcat_game_api::{GameDescriptor, GAME_ADAPTER_CONTRACT_ID};
use mintcat_runtime_release::PackageRecord;

pub(crate) fn signed_game(record: &PackageRecord) -> Result<Option<GameInventory>> {
    let games = record.manifest.contributes.get("games").and_then(Value::as_array);
    let adapter = record.manifest.provides.iter().any(|p| p.contract_id == GAME_ADAPTER_CONTRACT_ID);
    if !adapter { return Ok(None); }
    let games = games.ok_or_else(|| UpdateError::Blocked("game adapter requires a complete signed contributes.games descriptor".into()))?;
    require(games.len() == 1, "a module package must identify one independent game adapter, not multiple host game branches")?;
    let value = &games[0];
    let descriptor: GameDescriptor = serde_json::from_value(value.clone())?;
    descriptor.validate().map_err(|e| UpdateError::Blocked(format!("signed game descriptor invalid: {e}")))?;
    Ok(Some(GameInventory { game_id: descriptor.game_id, display_name: descriptor.display_name, descriptor: value.clone() }))
}
