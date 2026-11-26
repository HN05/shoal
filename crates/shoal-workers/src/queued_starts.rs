//! The daemon's worker for an admitted agent queue entry.
mod launch;
mod restore;

use anyhow::Result;

use crate::client::queue;
use crate::{client::connection::CommandPaths, state::QueueKind};
use shoal_ids::queue_entry_ids::QueueEntryId;

/// Start the admitted queue `entry`: open its launch's place, or restore its
/// stopped agent, freeing its slot with the error when that fails.
pub async fn run(paths: &CommandPaths, entry: QueueEntryId) -> Result<i32> {
    let started = start(paths, &entry).await;
    if let Err(error) = &started {
        fail(paths, &entry, error).await;
    }
    started.map(|()| 0)
}

async fn start(paths: &CommandPaths, entry: &QueueEntryId) -> Result<()> {
    let queued = queue::entries(paths)
        .await?
        .into_iter()
        .find(|queued| queued.id == *entry)
        .ok_or_else(|| anyhow::anyhow!("queue entry no longer exists"))?;
    match queued.kind {
        QueueKind::Launch => launch::start(paths, entry).await,
        QueueKind::Restore => restore::start(paths, &queued).await,
        QueueKind::Job => anyhow::bail!("the daemon starts a CI job itself"),
        QueueKind::Command => anyhow::bail!("a command's own wrapper starts it"),
    }
}

/// Free a starting entry's slot after its launch failed; the daemon notifies
/// the user with `error`.
pub async fn fail(paths: &CommandPaths, entry: &QueueEntryId, error: &anyhow::Error) {
    let failed = queue::fail(paths, entry.clone(), format!("{error:#}")).await;
    if let Err(error) = failed {
        eprintln!("warning: cannot release the agent queue slot: {error:#}");
    }
}
