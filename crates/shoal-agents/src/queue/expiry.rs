//! Atomic expiry of starting queue entries and its reportable failures.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::params;

use crate::state::{QueueKind, QueueState};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;

/// Why a launch that expired failed.
pub const ENDED: &str = "its launch ended before its agent started";

/// A start that expired without completing its handoff: a launch that its
/// recorded space keeps as failed, one that recorded none, by its name
/// alone, or a restore with its space's ID.
#[derive(Debug, PartialEq, Eq)]
pub enum FailedStart {
    KeptLaunch(SpaceId),
    UnopenedLaunch(SpaceName),
    UnstartedRestore(SpaceName, SpaceId),
}

/// End unheld, daemon-started entries older than `cutoff`: a launch stays
/// on its recorded space as failed, and anything else goes. Return
/// reportable failures, which the caller reports only once its transaction
/// commits.
pub fn expire_starts(
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
    for (id, name, kind, space) in stale {
        if held.contains(&id) {
            continue;
        }
        let failure = match (kind, space) {
            (QueueKind::Launch, _) => expire_launch(tx, &id)?,
            (QueueKind::Restore, Some(space)) => Some(FailedStart::UnstartedRestore(name, space)),
            // Jobs carry no start environment, a restore is always of a
            // space's agent, and the queue keeps no command's entry.
            (
                QueueKind::Job | QueueKind::Restore | QueueKind::Command | QueueKind::SeedBuild,
                _,
            ) => None,
        };
        tx.execute("DELETE FROM agent_queue WHERE id=?1", [&id])?;
        failures.extend(failure);
    }
    Ok(failures)
}

/// End the expired launch `id`, which its recorded space keeps as failed.
fn expire_launch(tx: &rusqlite::Transaction<'_>, id: &QueueEntryId) -> Result<Option<FailedStart>> {
    let failed = super::failures::end_start(tx, id, ENDED)?;
    Ok(failed.map(|failed| match (failed.space, failed.kept) {
        (Some(space), true) => FailedStart::KeptLaunch(space),
        _ => FailedStart::UnopenedLaunch(failed.name),
    }))
}

#[cfg(test)]
mod tests;
