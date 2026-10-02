use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use thiserror::Error;

use crate::RuntimeLayout;

#[derive(Debug, Error)]
pub enum KernelDatabaseError {
    #[error("runtime layout is not initialized")]
    UninitializedLayout,
    #[error("kernel database failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("kernel database migration {owner} expected version {expected}, got {actual}")]
    MigrationGap {
        owner: String,
        expected: u32,
        actual: u32,
    },
    #[error("kernel database mutex is poisoned")]
    Poisoned,
}

#[derive(Clone)]
pub struct KernelDatabase {
    path: PathBuf,
    connection: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for KernelDatabase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KernelDatabase")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl KernelDatabase {
    pub fn open(layout: &RuntimeLayout) -> Result<Self, KernelDatabaseError> {
        if !layout.initialized {
            return Err(KernelDatabaseError::UninitializedLayout);
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let connection = Connection::open_with_flags(&layout.kernel_database, flags)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS kernel_schema_migrations (
               owner TEXT NOT NULL,
               version INTEGER NOT NULL,
               applied_at_ms INTEGER NOT NULL,
               PRIMARY KEY (owner, version)
             );",
        )?;
        Ok(Self {
            path: layout.kernel_database.clone(),
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn connection(&self) -> Result<MutexGuard<'_, Connection>, KernelDatabaseError> {
        self.connection
            .lock()
            .map_err(|_| KernelDatabaseError::Poisoned)
    }

    pub fn apply_migration(
        &self,
        owner: &str,
        version: u32,
        sql: &str,
    ) -> Result<bool, KernelDatabaseError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest: u32 = transaction.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM kernel_schema_migrations WHERE owner = ?1",
            [owner],
            |row| row.get(0),
        )?;
        if latest >= version {
            transaction.commit()?;
            return Ok(false);
        }
        let expected = latest + 1;
        if version != expected {
            return Err(KernelDatabaseError::MigrationGap {
                owner: owner.to_owned(),
                expected,
                actual: version,
            });
        }
        transaction.execute_batch(sql)?;
        transaction.execute(
            "INSERT INTO kernel_schema_migrations(owner, version, applied_at_ms)
             VALUES (?1, ?2, CAST(strftime('%s', 'now') AS INTEGER) * 1000)",
            (owner, version),
        )?;
        transaction.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{KernelDatabase, RuntimeEnvironment, RuntimePaths};

    #[test]
    fn applies_owned_migrations_once_in_the_isolated_kernel_database() {
        let sandbox = tempdir().unwrap();
        let layout = RuntimePaths::new(sandbox.path().join("data"))
            .unwrap()
            .initialize(RuntimeEnvironment::Test)
            .unwrap();
        let database = KernelDatabase::open(&layout).unwrap();
        assert!(database
            .apply_migration(
                "fixture",
                1,
                "CREATE TABLE fixture_records(id TEXT PRIMARY KEY);"
            )
            .unwrap());
        assert!(!database
            .apply_migration(
                "fixture",
                1,
                "CREATE TABLE fixture_records(id TEXT PRIMARY KEY);"
            )
            .unwrap());
        let count: u64 = database
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM kernel_schema_migrations WHERE owner = 'fixture'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(database.path(), layout.kernel_database);
    }
}
