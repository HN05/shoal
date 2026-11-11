//! `shoal queue start --attach`: wait until the started entry's agent runs
//! in a session, then attach this terminal to that session.
use crate::client::space_ids::SpaceId;
use std::time::Duration;

use anyhow::{Result, bail, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        commands::{queued_agents, sessions},
        context::Context,
    },
    model::QueueEntry,
    state::QueueKind,
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

/// Attach to the session of the started `entry`'s agent once it has one.
pub(super) async fn started(ctx: &Context, entry: &QueueEntry) -> Result<i32> {
    let (workspace, session) = agent_terminal_session(ctx, entry).await?;
    sessions::attach(ctx, Some(SpaceSelector::id(&workspace)), Some(session)).await
}

/// The ID of the workspace `entry` starts its agent in and the name of the
/// terminal session the agent runs in, once it runs there; other sessions of the
/// workspace, such as a shell left after a failed setup, do not count. A
/// launch learns its workspace while it starts. The daemon records the
/// agent where it runs before it takes the entry out of the queue, so an
/// entry gone before the sessions were read without an agent session among
/// them did not start in one.
async fn agent_terminal_session(ctx: &Context, entry: &QueueEntry) -> Result<(SpaceId, String)> {
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut workspace = entry.workspace().cloned();
    let existing: Vec<SpaceId> = (crate::client::workspaces::list(&ctx.paths).await?)
        .into_iter()
        .map(|workspace| workspace.id)
        .collect();
    loop {
        let current = (queued_agents::entries(ctx).await?)
            .into_iter()
            .find(|queued| queued.id == entry.id);
        if let Some(opened) = current.as_ref().and_then(QueueEntry::workspace) {
            workspace = Some(opened.to_owned());
        }
        if workspace.is_none() && current.is_none() {
            let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
            workspace = opened_workspace(workspaces, entry, &existing)?;
        }
        match &workspace {
            Some(id) => {
                if let Some(session) = sessions::agent_terminal_session(ctx, id).await? {
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

/// The ID of the workspace the launch `entry` opened, once its agent took
/// the entry out of the queue before a poll saw the workspace it recorded:
/// the one of `workspaces` in its repository with its name that was not
/// among the `existing` ones when the wait began. Names repeat, so several
/// such workspaces are refused rather than guessed between.
fn opened_workspace(
    workspaces: Vec<crate::model::Workspace>,
    entry: &QueueEntry,
    existing: &[SpaceId],
) -> Result<Option<SpaceId>> {
    let opened: Vec<_> = (workspaces.into_iter())
        .filter(|workspace| {
            workspace.repository_id == entry.repository_id
                && workspace.name == entry.name
                && !existing.contains(&workspace.id)
        })
        .collect();
    ensure!(
        opened.len() <= 1,
        "workspaces {} opened as {} meanwhile; attach to {}'s with shoal attach <number>",
        (opened.iter().map(|workspace| workspace.number.to_string()))
            .collect::<Vec<_>>()
            .join(", "),
        entry.name,
        entry.name
    );
    Ok(opened.into_iter().next().map(|workspace| workspace.id))
}

#[cfg(test)]
mod tests;
