//! `shoal queue start --attach`, and `shoal attach` of an agent Shoal
//! stopped: wait until the started entry's agent runs in a session, then
//! attach this terminal to that session.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use std::time::Duration;

use anyhow::{Result, bail, ensure};

use crate::client::queue;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        commands::{queued_agents, sessions},
        context::Context,
    },
    model::QueueEntry,
    sessions::HostName,
    state::{QueueKind, QueueState},
};

/// How long to wait for the agent's session, which setup comes before.
const WAIT: Duration = Duration::from_secs(600);
const POLL: Duration = Duration::from_millis(200);

/// Refuse to attach to the entry `selector` names when it is a CI job,
/// which has no session, before anything starts it.
pub(super) async fn require_session(ctx: &Context, selector: &str) -> Result<()> {
    let entries = queued_agents::entries(ctx).await?;
    let job = (entries.iter()).find(|entry| entry.kind == QueueKind::Job && entry.id == selector);
    match job {
        Some(job) => bail!("{} is a CI job, which has no session to attach", job.name),
        None => Ok(()),
    }
}

/// Start the queued restore of the agent Shoal stopped as `execution`,
/// unless it is already starting, and wait until it runs again in the
/// session with the key `session`, where its wrapper waits; `false` when
/// no restore of it waits in the queue.
pub(in crate::cli::commands) async fn resume_stopped(
    ctx: &Context,
    execution: &ExecutionId,
    session: (HostName, &str),
) -> Result<bool> {
    let entries = queued_agents::entries(ctx).await?;
    let restore = entries.into_iter().find(|entry| {
        entry.kind == QueueKind::Restore && entry.execution.as_ref() == Some(execution)
    });
    let Some(restore) = restore else {
        return Ok(false);
    };
    let entry = match restore.state {
        QueueState::Queued => queue::start(&ctx.paths, restore.id.to_string()).await?,
        QueueState::Starting => restore,
    };
    ctx.emit(&format!("Resuming {}", entry.name), &entry)?;
    agent_terminal_session(ctx, &entry, Some(session)).await?;
    Ok(true)
}

/// Attach to the session of the started `entry`'s agent once it has one.
pub(super) async fn started(ctx: &Context, entry: &QueueEntry) -> Result<i32> {
    let (workspace, session) = agent_terminal_session(ctx, entry, None).await?;
    sessions::attach(ctx, Some(SpaceSelector::id(&workspace)), Some(session)).await
}

/// The ID of the workspace `entry` starts its agent in and the name of the
/// terminal session the agent runs in, once it runs there; other sessions of the
/// workspace, such as a shell left after a failed setup, do not count. A
/// launch learns its workspace while it starts. The daemon records the
/// agent where it runs before it takes the entry out of the queue, so an
/// entry gone before the sessions were read without an agent session among
/// them did not start in one. Only the session with `key` counts when
/// given.
async fn agent_terminal_session(
    ctx: &Context,
    entry: &QueueEntry,
    key: Option<(HostName, &str)>,
) -> Result<(SpaceId, String)> {
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut workspace = entry.workspace().cloned();
    loop {
        let current = (queued_agents::entries(ctx).await?)
            .into_iter()
            .find(|queued| queued.id == entry.id);
        if let Some(opened) = current.as_ref().and_then(QueueEntry::workspace) {
            workspace = Some(opened.to_owned());
        }
        if workspace.is_none() && current.is_none() {
            // The launch left the queue before a poll saw its workspace;
            // names repeat, so the daemon says which one its agent took.
            workspace = queue::started_workspace(&ctx.paths, entry.id.clone()).await?;
        }
        match &workspace {
            Some(id) => {
                if let Some(session) = sessions::agent_terminal_session(ctx, id, key).await? {
                    return Ok((id.clone(), session));
                }
                ensure!(
                    current.is_some(),
                    "{} has no agent session to attach; shoal status {id} shows whether its agent runs, and shoal alerts why it did not start",
                    entry.name
                );
            }
            None if current.is_none() => {
                bail!("{} did not start; shoal alerts shows why", entry.name)
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
