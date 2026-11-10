//! Queuing the restores of stopped agents, which the daemon starts as the
//! agent limits allow, or at once with `--now`.
use crate::client::execution_ids::ExecutionId;
use anyhow::{Context as _, Result, ensure};

use super::{chats, commands, permission_change, stale, targets::Selected};
use crate::client::output::{
    batch::Failure,
    resume::{Queued, RestoredChat},
};
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, ui},
    config::permissions::PermissionChange,
    execution::recovery::Saved,
    model::{QueueEntry, Workspace},
    protocol::ConfigTarget,
    state::QueueState,
    terminal,
};

/// How a resume starts the restores it does not run in this terminal.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Queuing {
    /// Queued as the limits allow, except a workspace's last session, or
    /// for `now` started by this process where each stopped or in the
    /// background.
    Here { now: bool },
    /// Queued for the daemon to start, which a noninteractive process
    /// needs, at once regardless of the limits for `now`.
    Daemon { now: bool },
}

impl Queuing {
    /// How a process with `ctx` starts restores: a noninteractive one has no
    /// terminal to start them beside.
    pub(super) fn of(ctx: &Context, now: bool) -> Self {
        match ctx.interactive() {
            true => Self::Here { now },
            false => Self::Daemon { now },
        }
    }
}

/// Queue the restores of the stopped agents and last sessions of the
/// `selected` workspaces below this process's, only `execution`'s when
/// given, and restore the `selected` chats in a background place, under
/// `change` when given; the daemon refuses those this process or one
/// below it did not stop, before any restore, and starts the workspaces'
/// agents detached.
pub(super) async fn queue_below(
    ctx: &Context,
    selected: Selected,
    execution: Option<ExecutionId>,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let mut stopped = Vec::new();
    for selector in selected.workspaces.unwrap_or_default() {
        stopped.push(stopped_agents(ctx, selector, execution.as_ref()).await?);
    }
    for chat in &selected.chats {
        crate::client::chats::check_restore(&ctx.paths, &chat.id).await?;
    }
    permission_change::apply(ctx, &stopped, &selected.chats, change).await?;
    let (chats, failed) = chats::restore_each(ctx, &selected.chats, change).await;
    queue_all(ctx, stopped, chats, failed, false).await
}

/// Executions stopped so their workspaces could move, by workspace.
pub(in crate::cli::commands) type Stopped =
    std::collections::BTreeMap<SpaceId, std::collections::BTreeSet<ExecutionId>>;

/// Queue the restores of the agents among `stopped`; their workspaces'
/// stopped commands go to them, and those of workspaces without one stay
/// for `shoal resume`.
pub(in crate::cli::commands) async fn queue_stopped(
    ctx: &Context,
    stopped: &Stopped,
) -> Result<Queued> {
    let mut queued = Vec::new();
    for (workspace, executions) in stopped {
        let workspace =
            crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(workspace))
                .await?
                .workspace;
        let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
        saved.retain(|id| executions.contains(id));
        if !(saved.agents.is_empty() && saved.sessions.is_empty()) {
            queued.push((workspace, saved));
        }
    }
    Ok(queue_each(ctx, queued, false).await)
}

/// The stopped agents and last sessions of the workspace `selector` names,
/// only `execution`'s when given, refusing one without any. Its stopped
/// commands go to the restored agents.
async fn stopped_agents(
    ctx: &Context,
    selector: SpaceSelector,
    execution: Option<&ExecutionId>,
) -> Result<(Workspace, Saved)> {
    let workspace = crate::client::workspaces::inspect(&ctx.paths, selector.clone())
        .await?
        .workspace;
    let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
    if let Some(execution) = execution {
        saved.retain_execution(execution, true);
    }
    saved.retain_unattended();
    saved.commands.clear();
    ensure!(
        !(saved.agents.is_empty() && saved.sessions.is_empty()),
        "{} has no stopped agent to resume",
        workspace.name
    );
    Ok((workspace, saved))
}

/// Queue the restores of `workspace`'s stopped agents, only `execution`'s
/// when given, to start as `queuing` says, making `change` when given.
/// Without stopped agents, a last session continues at once, as resuming
/// the workspace by itself does not wait for a slot it never held: where it
/// stopped or in the background, or for a noninteractive process through
/// the queue, started now.
pub(super) async fn queue_workspace(
    ctx: &Context,
    workspace: SpaceSelector,
    execution: Option<ExecutionId>,
    queuing: Queuing,
    change: Option<&PermissionChange>,
) -> Result<i32> {
    let workspace = crate::client::workspaces::inspect(&ctx.paths, workspace.clone())
        .await?
        .workspace;
    let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
    if let Some(execution) = &execution {
        saved.retain_execution(execution, true);
    }
    ensure!(
        !saved.is_empty(),
        "no stopped agents or commands for this workspace"
    );
    let last_session = saved.agents.is_empty() && !saved.sessions.is_empty();
    let now = match queuing {
        Queuing::Here { .. } if last_session => {
            return super::workspace::continue_last_session(ctx, &workspace, saved, false, change)
                .await;
        }
        Queuing::Here { .. } => false,
        Queuing::Daemon { now } => now || last_session,
    };
    let stopped = vec![(workspace, saved)];
    if matches!(queuing, Queuing::Daemon { .. }) {
        ensure_daemon_starts(ctx, &stopped).await?;
    }
    permission_change::apply(ctx, &stopped, &[], change).await?;
    queue_all(ctx, stopped, Vec::new(), Vec::new(), now).await
}

/// Refuse, before any is queued, the restores of `stopped` agents that the
/// daemon would start from the background when no host can take them there;
/// a waiting wrapper restores its agent where it stopped.
pub(super) async fn ensure_daemon_starts(
    ctx: &Context,
    stopped: &[(Workspace, Saved)],
) -> Result<()> {
    for (workspace, saved) in stopped {
        if !saved.restores_outside_wrappers() {
            continue;
        }
        let settings = crate::client::workspaces::settings(
            &ctx.paths,
            ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
        )
        .await?;
        crate::cli::terminal::ensure_background(&settings).with_context(|| {
            format!(
                "cannot restore the agents of {} in the background; pass --focus to restore one here",
                workspace.name
            )
        })?;
    }
    Ok(())
}

/// Queue the restore of every agent and last session `stopped` saves,
/// starting each at once regardless of the limits when `now`, and report
/// the commands of workspaces without one, going on after one fails, with
/// the stopped `chats` restored and the resumes already `failed`.
pub(super) async fn queue_all(
    ctx: &Context,
    stopped: Vec<(Workspace, Saved)>,
    chats: Vec<RestoredChat>,
    mut failed: Vec<Failure>,
    now: bool,
) -> Result<i32> {
    let mut queued = queue_each(ctx, stopped, now).await;
    queued.chats = chats;
    failed.append(&mut queued.failed);
    queued.failed = failed;
    ctx.show(&queued, |queued| {
        if let Some(summary) = queued.summary() {
            outln!("{summary}");
        }
    })?;
    Ok(if queued.failed.is_empty() { 0 } else { 1 })
}

/// Operations on the client type [`Queued`] that only the `shoal` command uses.
pub(crate) trait QueuedExt {
    /// What the user reads once restores are queued, `None` without any.
    fn summary(&self) -> Option<String>;
}

impl QueuedExt for Queued {
    fn summary(&self) -> Option<String> {
        let starting = (self.queued.iter()).all(|entry| entry.state == QueueState::Starting);
        (!self.queued.is_empty()).then(|| summary(self.queued.len(), starting))
    }
}

/// Queue the restore of every agent and last session `stopped` saves,
/// starting each at once when `now`, and report the commands of workspaces
/// without one that restores outside a waiting wrapper, going on after one
/// fails, which it reports.
pub(super) async fn queue_each(
    ctx: &Context,
    stopped: Vec<(Workspace, Saved)>,
    now: bool,
) -> Queued {
    let mut queued = Vec::new();
    let mut commands = Vec::new();
    let mut failed = Vec::new();
    for (workspace, mut saved) in stopped {
        if !saved.restores_outside_wrappers() {
            let _claims = commands::claim(&mut saved.commands);
            match commands::report(ctx, &workspace, &saved.commands) {
                Ok(report) => commands.push(report),
                Err(error) => failed.push(ui::failure(ctx, "resume", &workspace.name, &error)),
            }
        }
        for (execution, _) in saved.agents.iter().chain(&saved.sessions) {
            match queue(ctx, &workspace, execution, now).await {
                Ok(entry) => queued.push(entry),
                Err(error) => failed.push(ui::failure(ctx, "resume", &workspace.name, &error)),
            }
        }
    }
    Queued {
        queued,
        commands,
        chats: Vec::new(),
        failed,
    }
}

/// What the user reads once `count` restores are queued, or `starting`.
fn summary(count: usize, starting: bool) -> String {
    let agents = if count == 1 { "agent" } else { "agents" };
    let done = if starting { "Starting" } else { "Queued" };
    format!("{done} {count} {agents}; shoal queue shows them")
}

/// Queue the restore of the agent stopped as `execution` in `workspace`,
/// once an execution a daemon restart left unknown is proven ended,
/// starting it at once when `now`.
pub(super) async fn queue(
    ctx: &Context,
    workspace: &Workspace,
    execution: &ExecutionId,
    now: bool,
) -> Result<QueueEntry> {
    let inspection =
        crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace.id)).await?;
    stale::reconcile(ctx, &inspection, Some(execution)).await?;
    let (workspace, execution) = (workspace.id.clone(), execution.to_owned());
    crate::client::queue::restore(
        &ctx.paths,
        SpaceSelector::id(&workspace),
        execution,
        terminal::start_environment(),
        now,
    )
    .await
}

#[cfg(test)]
mod tests;
