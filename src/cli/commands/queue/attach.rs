//! `shoal queue start --attach`: wait until the started entry's agent has a
//! session, then attach this terminal to it.
use std::time::Duration;

use anyhow::{Result, bail, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        commands::{agent_queue, sessions},
        context::Context,
    },
    model::QueueEntry,
    state::QueueKind,
};

/// How long to wait for the session; setup runs inside it, so it opens
/// before the agent starts.
const WAIT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(200);

/// Attach to the session of the started `entry`'s agent once it has one.
pub(super) async fn started(ctx: &Context, entry: &QueueEntry) -> Result<i32> {
    ensure!(
        entry.kind != QueueKind::Job,
        "{} is a CI job, which has no session to attach",
        entry.name
    );
    let workspace = session_workspace(ctx, entry).await?;
    sessions::attach(ctx, Some(SpaceSelector::id(&workspace)), None).await
}

/// The ID of the workspace `entry` starts its agent in, once that workspace
/// has a session to attach. A launch learns its workspace while it starts,
/// and a launch that failed leaves the queue without one.
async fn session_workspace(ctx: &Context, entry: &QueueEntry) -> Result<String> {
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut workspace = entry.workspace().map(str::to_owned);
    loop {
        let current = (agent_queue::entries(ctx).await?)
            .into_iter()
            .find(|queued| queued.id == entry.id);
        if let Some(opened) = current.as_ref().and_then(QueueEntry::workspace) {
            workspace = Some(opened.to_owned());
        }
        if workspace.is_none() && current.is_none() {
            workspace = opened_workspace(ctx, entry).await?;
        }
        match &workspace {
            Some(id) if sessions::has_session(ctx, id).await? => return Ok(id.clone()),
            None if current.is_none() => {
                bail!("{} did not start; shoal alerts shows why", entry.name)
            }
            _ => {}
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "{0} has no agent session after {1} seconds; shoal attach {0} attaches once it has one",
            entry.name,
            WAIT.as_secs()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The ID of the workspace the launch `entry` opened, found by its name in
/// its repository once its agent took the entry out of the queue before a
/// poll saw the workspace it recorded.
async fn opened_workspace(ctx: &Context, entry: &QueueEntry) -> Result<Option<String>> {
    let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
    Ok(workspaces
        .into_iter()
        .find(|workspace| {
            workspace.repository_id == entry.repository_id && workspace.name == entry.name
        })
        .map(|workspace| workspace.id))
}
