use std::{collections::BTreeSet, time::{SystemTime, UNIX_EPOCH}};

use rusqlite::{params, TransactionBehavior};

use crate::{LibraryError, OwnerStore};

impl OwnerStore { /// The daemon calls this only after file-journal recovery. A guard whose
/// Profile remains in an unresolved journal is retained, regardless of age.
pub fn recover_profile_content_guards(
    &self,
    generation: &str,
    unresolved_profiles: &BTreeSet<String>,
) -> Result<usize, LibraryError> {
    if generation.trim().is_empty() {
        return Err(LibraryError::InvalidRecord("recovery generation is required".to_owned()));
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)
        .map_err(|error| LibraryError::InvalidRecord(error.to_string()))?.as_millis();
    let now = i64::try_from(now).map_err(|error| LibraryError::InvalidRecord(error.to_string()))?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut statement = transaction.prepare(
        "SELECT guard_id, profile_id, fence_epoch FROM library_profile_content_guards
         WHERE state = 'active' AND owner_generation <> ?1",
    )?;
    let guards = statement.query_map([generation], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
    })?.collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut released = 0;
    for (guard, profile, epoch) in guards {
        if unresolved_profiles.contains(&profile) { continue; }
        if epoch == i64::MAX {
            return Err(LibraryError::InvalidRecord("content guard fence epoch is exhausted".to_owned()));
        }
        let changed = transaction.execute(
            "UPDATE library_profile_content_guards SET state = 'released', released_at_ms = ?4
             WHERE guard_id = ?1 AND fence_epoch = ?2 AND state = 'active'
               AND owner_generation <> ?3",
            params![guard, epoch, generation, now],
        )?;
        if changed == 1 {
            transaction.execute(
                "UPDATE library_profiles SET lease_epoch = MAX(lease_epoch, ?2) + 1
                 WHERE profile_id = ?1",
                params![profile, epoch],
            )?;
            released += 1;
        }
    }
    transaction.commit()?;
    Ok(released)
} }
