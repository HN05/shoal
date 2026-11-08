//! Atomic expiry of starting queue entries and its reportable failures.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::params;

use crate::state::{QueueKind, QueueState};

/// A start that expired without completing its handoff.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FailedStart {
    UnopenedLaunch(String),
    UnstartedRestore(String),
}

/// Delete unheld, daemon-started entries older than `cutoff`. Opened launches
/// expire silently. Return reportable failures, which the caller reports
/// only once its transaction commits.
pub(super) fn expire_starts(
    tx: &rusqlite::Transaction<'_>,
    held: &HashSet<String>,
    cutoff: crate::time::UnixTime,
) -> Result<Vec<FailedStart>> {
    let stale = tx
        .prepare(
            "SELECT id,name,kind,workspace_id IS NULL FROM agent_queue
             WHERE state=?1 AND env IS NOT NULL AND seen_at<?2",
        )?
        .query_map(params![QueueState::Starting, cutoff], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<(String, String, QueueKind, bool)>>>()?;
    let mut failures = Vec::new();
    for (id, name, kind, unopened) in stale {
        if held.contains(&id) {
            continue;
        }
        tx.execute("DELETE FROM agent_queue WHERE id=?1", [&id])?;
        let failure = match kind {
            QueueKind::Launch if unopened => FailedStart::UnopenedLaunch(name),
            // Opened launches expire silently; jobs carry no start environment.
            QueueKind::Launch | QueueKind::Job => continue,
            QueueKind::Restore => FailedStart::UnstartedRestore(name),
        };
        failures.push(failure);
    }
    Ok(failures)
}

#[cfg(test)]
mod tests;
