//! Waking an asleep agent from the place its wrapper waits in: starting its
//! queued restore now, as `shoal queue start` does, whatever the limits.
use anyhow::{Result, bail};
use shoal_ids::execution_ids::ExecutionId;

use crate::{
    client::{connection::CommandPaths, queue},
    state::{QueueKind, QueueState},
};

/// Start the queued restore of the agent `execution` ran, unless it already
/// starts; refused while no restore of it waits in the queue.
pub(super) async fn wake(paths: &CommandPaths, execution: &ExecutionId) -> Result<()> {
    let entries = queue::entries(paths).await?;
    let restore = (entries.into_iter()).find(|entry| {
        entry.kind == QueueKind::Restore && entry.execution.as_ref() == Some(execution)
    });
    match restore {
        Some(entry) if entry.state == QueueState::Queued => {
            queue::start(paths, entry.id.to_string()).await.map(drop)
        }
        Some(_) => Ok(()),
        None => bail!("its restore does not wait in the agent queue yet"),
    }
}
