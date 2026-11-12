//! Atomic expiry of starting queue entries and its reportable failures.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::params;

use crate::client::queue::QueueEntryId;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceName;
use crate::state::{QueueKind, QueueState};

/// A start that expired without completing its handoff: a launch that
/// opened no workspace, by its name alone, or a restore with its
/// workspace's ID.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FailedStart {
    UnopenedLaunch(SpaceName),
    UnstartedRestore(SpaceName, SpaceId),
}

/// Delete unheld, daemon-started entries older than `cutoff`. Opened launches
/// expire silently. Return reportable failures, which the caller reports
/// only once its transaction commits.
pub(super) fn expire_starts(
    tx: &rusqlite::Transaction<'_>,
    held: &HashSet<QueueEntryId>,
    cutoff: crate::time::UnixTime,
) -> Result<Vec<FailedStart>> {
    let stale = tx
        .prepare(
            "SELECT id,name,kind,space_id FROM agent_queue
             WHERE state=?1 AND env IS NOT NULL AND seen_at<?2",
        )?
        .query_map(params![QueueState::Starting, cutoff], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<(QueueEntryId, SpaceName, QueueKind, Option<SpaceId>)>>>(
        )?;
    let mut failures = Vec::new();
    for (id, name, kind, workspace) in stale {
        if held.contains(&id) {
            continue;
        }
        tx.execute("DELETE FROM agent_queue WHERE id=?1", [&id])?;
        let failure = match (kind, workspace) {
            (QueueKind::Launch, None) => FailedStart::UnopenedLaunch(name),
            // Opened launches expire silently; jobs carry no start environment.
            (QueueKind::Launch | QueueKind::Job, _) => continue,
            (QueueKind::Restore, Some(workspace)) => FailedStart::UnstartedRestore(name, workspace),
            // A restore is always of a workspace's agent.
            (QueueKind::Restore, None) => continue,
        };
        failures.push(failure);
    }
    Ok(failures)
}

#[cfg(test)]
mod tests;
