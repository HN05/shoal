//! Starting the queued restore of an agent an attach wakes, and waiting
//! until a started queue entry's agent runs in a session.
use std::time::Duration;

use anyhow::{Result, bail, ensure};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use super::{Attaching, Target, sessions::Steps};
use crate::{
    client::{connection::CommandPaths, queue},
    model::QueueEntry,
    state::{QueueKind, QueueState},
};

/// How long to wait for the agent's session, which setup comes before.
pub(super) const WAIT: Duration = Duration::from_secs(600);
pub(super) const POLL: Duration = Duration::from_millis(200);

impl<A: Attaching> Steps<'_, A> {
    /// Start the queued restore of the agent Shoal stopped as `execution`,
    /// unless it is already starting, and wait until it runs again in its
    /// session, where its wrapper waits; `false` when no restore of it
    /// waits in the queue.
    pub async fn resume_stopped(&self, execution: &ExecutionId) -> Result<bool> {
        self.resume_queued(|entry| {
            entry.kind == QueueKind::Restore && entry.execution.as_ref() == Some(execution)
        })
        .await
    }

    /// Start the queued restore of an agent of the space `space`, as
    /// [`Self::resume_stopped`] does; `false` when none waits in the queue.
    pub async fn resume_space(&self, space: &SpaceId) -> Result<bool> {
        self.resume_queued(|entry| entry.kind == QueueKind::Restore && entry.space() == Some(space))
            .await
    }

    /// Wait until the agent a launch is starting in the space `space` runs
    /// in its session, as one a start that attaches to it at once asked to
    /// start now; `false` when no launch is starting there.
    pub(super) async fn await_starting_launch(&self, space: &SpaceId) -> Result<bool> {
        self.resume_queued(|entry| {
            let starting = entry.kind == QueueKind::Launch && entry.state == QueueState::Starting;
            starting && entry.space() == Some(space)
        })
        .await
    }

    /// Start the first queued entry that `picks` picks, unless it is
    /// already starting, and wait until its agent runs in a session,
    /// failing at once when it leaves the queue without one; `false` when
    /// none waits.
    async fn resume_queued(&self, picks: impl Fn(&QueueEntry) -> bool) -> Result<bool> {
        let entries = queue::entries(self.paths).await?;
        let Some(restore) = entries.into_iter().find(picks) else {
            return Ok(false);
        };
        let entry = match restore.state {
            QueueState::Queued => queue::start(self.paths, restore.id.to_string()).await?,
            QueueState::Starting => restore,
            QueueState::Failed => bail!("the restore of {} failed", restore.name),
        };
        self.attaching.resuming(&entry)?;
        entry_session(self.paths, &entry, entry.execution.as_ref()).await?;
        Ok(true)
    }
}

/// The ID of the workspace `entry` starts its agent in and the name of the
/// terminal session the agent runs in, once it runs there; other sessions
/// of the workspace, such as a shell left after a failed setup, do not
/// count. A launch learns its workspace while it starts. The daemon records
/// the agent where it runs before it takes the entry out of the queue, so
/// an entry gone before the sessions were read without an agent session
/// among them did not start in one. Only `execution`'s session counts when
/// given.
pub async fn entry_session(
    paths: &CommandPaths,
    entry: &QueueEntry,
    execution: Option<&ExecutionId>,
) -> Result<(SpaceId, String)> {
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut workspace = entry.space().cloned();
    loop {
        let current = (queue::entries(paths).await?)
            .into_iter()
            .find(|queued| queued.id == entry.id);
        if let Some(failed) = current
            .as_ref()
            .filter(|queued| queued.state == QueueState::Failed)
        {
            let error = failed.error.as_deref().unwrap_or("its start failed");
            bail!(
                "{} did not start: {error}; shoal resume starts it again",
                entry.name
            );
        }
        if let Some(opened) = current.as_ref().and_then(QueueEntry::space) {
            workspace = Some(opened.to_owned());
        }
        if workspace.is_none() && current.is_none() {
            // The launch left the queue before a poll saw its workspace;
            // names repeat, so the daemon says which one its agent took.
            workspace = queue::started_workspace(paths, entry.id.clone()).await?;
        }
        match &workspace {
            Some(id) => {
                if let Some(session) = agent_session_name(paths, id, execution).await? {
                    return Ok((id.clone(), session));
                }
                ensure!(
                    current.is_some(),
                    "{} has no agent session to attach; shoal status {id} shows whether its agent runs, and shoal alert list why it did not start",
                    entry.name
                );
            }
            None if current.is_none() => {
                bail!("{} did not start; shoal alert list shows why", entry.name)
            }
            None => {}
        }
        let attach = workspace.as_ref().map_or("", SpaceId::as_str);
        ensure!(
            tokio::time::Instant::now() < deadline,
            "{} has no agent session after {} seconds; shoal attach {attach} attaches once it has one",
            entry.name,
            WAIT.as_secs()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The name of the terminal session `shoal attach` takes in which the
/// daemon recorded the agent of the space with the ID `id` running, only
/// `execution`'s when given.
async fn agent_session_name(
    paths: &CommandPaths,
    id: &SpaceId,
    execution: Option<&ExecutionId>,
) -> Result<Option<String>> {
    let target = Target::by_id(paths, id.clone()).await?;
    let sessions = target.sessions(paths).await?;
    let session = super::sessions::agent_session(sessions, execution);
    Ok(session.map(|session| session.name))
}
