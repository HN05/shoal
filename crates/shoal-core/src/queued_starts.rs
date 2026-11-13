//! The daemon's worker for an admitted agent queue entry, which has no
//! terminal: it opens the place of a launch where the terminal that queued
//! it would have, or restores a stopped agent where it stopped or in the
//! background. What runs in that place is the CLI's start or restore.
mod launch;
mod restore;

use anyhow::Result;

use crate::client::queue::{self, QueueEntryId};
use crate::{paths::CommandPaths, state::QueueKind};

pub use launch::{plan as launch_plan, start_admitted as start_admitted_launch};
pub use restore::start_chat as restore_chat_in_background;

/// Start the admitted queue `entry`: open its launch's place, or restore its
/// stopped agent, freeing its slot with the error when that fails.
pub async fn run(paths: &CommandPaths, entry: QueueEntryId) -> Result<i32> {
    let started = start(paths, &entry).await;
    if let Err(error) = &started {
        fail(paths, &entry, error).await;
    }
    started.map(|()| 0)
}

/// Start `entry` by its kind.
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
