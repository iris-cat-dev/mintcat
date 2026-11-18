use super::*;

impl UpdatesCoordinator {
    /// One durable cutover: remove UI process leases, archive non-game scopes, and
    /// preserve every signed generation/owner snapshot used by receipts or recovery.
    pub(super) fn migrate_legacy_state(&self) -> Result<()> {
        let mut connection = self.inner.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let json: String = transaction.query_row("SELECT state_json FROM update_state WHERE id=1", [], |row| row.get(0))?;
        let mut value: Value = serde_json::from_str(&json)?;
        let version = value.get("schema_version").and_then(Value::as_u64).unwrap_or(0);
        require(version <= 2, "unsupported persisted update state schema")?;
        if version == 2 { return Ok(()); }
        if let Some(leases) = value.get_mut("leases").and_then(Value::as_object_mut) {
            leases.retain(|_, lease| {
                let owner = lease.get("ownerId").and_then(Value::as_str).unwrap_or_default();
                let kind = lease.get("kind").and_then(Value::as_str);
                kind != Some("ui") && !(kind == Some("service")
                    && (owner.starts_with("source-generation:") || owner.starts_with("source-job:")
                        || owner.starts_with("backup-codec:") || owner.starts_with("native-owner:")))
            });
        }
        let mut state: UpdateState = serde_json::from_value(value)?;
        state.schema_version = 2;
        let obsolete: Vec<_> = state.environment.scopes.keys().filter(|scope| !scope.starts_with("game:")).cloned().collect();
        for scope in obsolete {
            let generation = state.environment.scopes.remove(&scope).expect("listed scope exists");
            state.archived_scopes.insert(scope.clone(), generation);
            state.scope_blockers.remove(&scope);
        }
        for operation in state.operations.values_mut() {
            if matches!(operation.phase, UpdatePhase::Confirmed | UpdatePhase::Rejected | UpdatePhase::RolledBack) { continue; }
            if !operation.backups.is_empty() || !operation.drained.is_empty() || !operation.migrated.is_empty() {
                let reason = format!("historical owner migration {} is unfinished; preserve its exact snapshots and recover with the original application before cutover", operation.operation_id);
                state.recovery_required = Some(reason.clone());
                state.recovery_operation = Some(operation.operation_id.clone());
                operation.phase = UpdatePhase::RecoveryRequired;
                operation.failure = Some(reason);
            } else if !operation.scope_id.starts_with("game:") {
                operation.phase = UpdatePhase::Rejected;
                operation.failure = Some("historical non-game activation retired; signed proof retained without executing replacement code".into());
            }
        }
        // A historical failure concerning owner data is not silently cleared.
        transaction.execute("UPDATE update_state SET state_json=?1 WHERE id=1", [serde_json::to_string(&state)?])?;
        transaction.execute("INSERT INTO update_events(event) VALUES('game-only-schema2-cutover')", [])?;
        transaction.commit()?;
        Ok(())
    }
}
