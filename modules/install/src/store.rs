use mintcat_runtime_kernel::KernelDatabase;
use rusqlite::{params, OptionalExtension, TransactionBehavior};

use crate::{ImmutableInstallPlan, InstallError};

const MIGRATION_OWNER: &str = "modules.install";
const MIGRATION: &str = r#"
CREATE TABLE install_plans (
  plan_id TEXT PRIMARY KEY,
  plan_hash TEXT NOT NULL UNIQUE,
  installation_id TEXT NOT NULL,
  profile_id TEXT NOT NULL,
  game_id TEXT NOT NULL,
  state TEXT NOT NULL,
  plan_json TEXT NOT NULL,
  receipt_id TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
CREATE INDEX install_plans_installation
  ON install_plans(installation_id, created_at_ms DESC);
CREATE INDEX install_plans_receipt
  ON install_plans(receipt_id);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoredPlanState {
    Prepared,
    Applying,
    Applied,
}

impl StoredPlanState {
    fn parse(value: &str) -> Result<Self, InstallError> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "applying" => Ok(Self::Applying),
            "applied" => Ok(Self::Applied),
            other => Err(InstallError::InvalidRequest(format!(
                "unknown persisted install plan state {other}"
            ))),
        }
    }
}

pub(crate) struct StoredPlan {
    pub plan: ImmutableInstallPlan,
    pub state: StoredPlanState,
    pub receipt_id: Option<String>,
}

#[derive(Clone)]
pub(crate) struct InstallPlanStore {
    database: KernelDatabase,
}

impl InstallPlanStore {
    pub fn open(database: KernelDatabase) -> Result<Self, InstallError> {
        database.apply_migration(MIGRATION_OWNER, 1, MIGRATION)?;
        Ok(Self { database })
    }

    pub fn insert(&self, plan: &ImmutableInstallPlan) -> Result<(), InstallError> {
        let connection = self.database.connection()?;
        connection.execute(
            "INSERT INTO install_plans(
               plan_id, plan_hash, installation_id, profile_id, game_id, state,
               plan_json, receipt_id, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'prepared', ?6, NULL, ?7, ?7)",
            params![
                plan.plan_id,
                plan.plan_hash,
                plan.profile.installation_id,
                plan.profile.profile_id,
                plan.profile.game_id,
                serde_json::to_string(plan)?,
                plan.created_at_ms,
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, plan_id: &str) -> Result<StoredPlan, InstallError> {
        let connection = self.database.connection()?;
        let row = connection
            .query_row(
                "SELECT state, plan_json, receipt_id FROM install_plans WHERE plan_id = ?1",
                [plan_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| InstallError::PlanNotFound(plan_id.to_owned()))?;
        Ok(StoredPlan {
            state: StoredPlanState::parse(&row.0)?,
            plan: serde_json::from_str(&row.1)?,
            receipt_id: row.2,
        })
    }

    pub fn begin_apply(&self, plan_id: &str, now_ms: i64) -> Result<StoredPlan, InstallError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = transaction
            .query_row(
                "SELECT state, plan_json, receipt_id FROM install_plans WHERE plan_id = ?1",
                [plan_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| InstallError::PlanNotFound(plan_id.to_owned()))?;
        let state = StoredPlanState::parse(&row.0)?;
        if state == StoredPlanState::Prepared {
            transaction.execute(
                "UPDATE install_plans SET state = 'applying', updated_at_ms = ?2
                 WHERE plan_id = ?1 AND state = 'prepared'",
                params![plan_id, now_ms],
            )?;
        }
        transaction.commit()?;
        Ok(StoredPlan {
            state,
            plan: serde_json::from_str(&row.1)?,
            receipt_id: row.2,
        })
    }

    pub fn mark_prepared(&self, plan_id: &str, now_ms: i64) -> Result<(), InstallError> {
        let connection = self.database.connection()?;
        connection.execute(
            "UPDATE install_plans SET state = 'prepared', updated_at_ms = ?2
             WHERE plan_id = ?1 AND state = 'applying'",
            params![plan_id, now_ms],
        )?;
        Ok(())
    }

    pub fn mark_applied(
        &self,
        plan_id: &str,
        receipt_id: &str,
        now_ms: i64,
    ) -> Result<(), InstallError> {
        let connection = self.database.connection()?;
        let changed = connection.execute(
            "UPDATE install_plans SET state = 'applied', receipt_id = ?2, updated_at_ms = ?3
             WHERE plan_id = ?1 AND state IN ('prepared', 'applying', 'applied')",
            params![plan_id, receipt_id, now_ms],
        )?;
        if changed != 1 {
            return Err(InstallError::PlanNotFound(plan_id.to_owned()));
        }
        Ok(())
    }

    pub fn plan_for_receipt(
        &self,
        receipt_id: &str,
    ) -> Result<Option<ImmutableInstallPlan>, InstallError> {
        let connection = self.database.connection()?;
        let raw = connection
            .query_row(
                "SELECT plan_json FROM install_plans WHERE receipt_id = ?1",
                [receipt_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        raw.map(|raw| serde_json::from_str(&raw).map_err(InstallError::from))
            .transpose()
    }

    pub fn plans(&self) -> Result<Vec<StoredPlan>, InstallError> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare("SELECT plan_id FROM install_plans ORDER BY created_at_ms, plan_id")?;
        let ids = statement.query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        ids.iter().map(|id| self.get(id)).collect()
    }

    pub fn unresolved_profiles(&self) -> Result<std::collections::BTreeSet<String>, InstallError> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT p.profile_id FROM install_plans p JOIN file_transactions t
             ON json_extract(t.journal_json, '$.planHash') = p.plan_hash
             WHERE t.kind = 'install' AND t.state IN ('prepared', 'applying', 'recovery-required')",
        )?;
        let profiles = statement.query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        Ok(profiles)
    }

    pub fn has_unresolved_plan(&self, plan_hash: &str) -> Result<bool, InstallError> {
        Ok(self.database.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_transactions
             WHERE kind = 'install' AND json_extract(journal_json, '$.planHash') = ?1
             AND state IN ('prepared', 'applying', 'recovery-required'))",
            [plan_hash],
            |row| row.get(0),
        )?)
    }

    pub fn active_receipt_plans(&self) -> Result<Vec<ImmutableInstallPlan>, InstallError> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            "SELECT p.plan_json FROM install_plans p JOIN install_receipts r
             ON r.receipt_id = p.receipt_id WHERE r.state = 'active'
             ORDER BY r.created_at_ms, r.receipt_id",
        )?;
        let plans = statement.query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        plans.iter().map(|plan| serde_json::from_str(plan).map_err(InstallError::from)).collect()
    }

    pub fn retained_plans(&self, now_ms: i64) -> Result<Vec<ImmutableInstallPlan>, InstallError> {
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            "SELECT p.plan_json FROM install_plans p
             WHERE p.state = 'applying'
                OR (p.state = 'prepared' AND json_extract(p.plan_json, '$.expiresAtMs') >= ?1)
                OR EXISTS (SELECT 1 FROM install_receipts r
                           WHERE r.receipt_id = p.receipt_id AND r.state = 'active')
             ORDER BY p.created_at_ms, p.plan_id",
        )?;
        let plans = statement.query_map([now_ms], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        plans.iter().map(|plan| serde_json::from_str(plan).map_err(InstallError::from)).collect()
    }
}
