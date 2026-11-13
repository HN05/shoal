//! Opening removed workspaces again from their removal records.
use crate::cli::commands::restore::agents::reports::QueuedExt;
use crate::client::execution_ids::ExecutionId;
use std::collections::BTreeSet;

use anyhow::Result;

use super::agents::queue::Stopped;
use super::checkpoints::branch_label;
use crate::client::output::restore::{Queued, Reopened};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        shell, terminal,
    },
    model::{RemovedWorkspace, RestoredWorkspace},
    state::WorkspaceState,
};

/// Open each of the `removed` workspaces again, set it up, link its PRs
/// and run its post-setup hook, in which the agents restored for it start;
/// `None` once a setup did not finish, which it reported. A workspace
/// opened alone becomes the directory of a shell that follows the caller.
pub(super) async fn reopen_each(
    ctx: &Context,
    removed: &[SpaceSelector],
) -> Result<Option<Vec<Reopened>>> {
    let mut reopened = Vec::new();
    for workspace in removed {
        let restore = crate::client::removal::restore(&ctx.paths, workspace.clone());
        let restored = ctx.progress("Restoring workspace", restore).await?;
        let Some(opened) = prepare_reopened(ctx, *restored).await? else {
            return Ok(None);
        };
        super::super::workspaces::run_post_setup(ctx, &opened.workspace).await?;
        reopened.push(opened);
    }
    if let [opened] = reopened.as_slice()
        && terminal::follows_caller(ctx)
    {
        shell::navigate(ctx, &opened.workspace.path)?;
    }
    Ok(Some(reopened))
}

/// Set up a workspace the daemon opened again from a removal record, and
/// link its PRs, then queue its stopped agents when `restore`; `action`
/// begins the report.
pub(in crate::cli::commands) async fn finish_reopen(
    ctx: &Context,
    restored: RestoredWorkspace,
    action: &str,
    restore: bool,
) -> Result<i32> {
    let Some(mut reopened) = prepare_reopened(ctx, restored).await? else {
        return Ok(1);
    };
    // The agents start in a workspace whose post-setup hook succeeded.
    if restore {
        super::super::workspaces::run_post_setup(ctx, &reopened.workspace).await?;
        reopened.queue_agents(ctx).await;
    }
    ctx.emit(&reopened.message(ctx, action), &reopened)?;
    if terminal::follows_caller(ctx) {
        shell::navigate(ctx, &reopened.workspace.path)?;
    }
    if !restore {
        super::super::workspaces::run_post_setup(ctx, &reopened.workspace).await?;
    }
    Ok(0)
}

/// Operations on the client type [`Reopened`] that only the `shoal` command uses.
pub(crate) trait ReopenedExt {
    /// The report of the reopening, which `action` begins.
    fn message(&self, ctx: &Context, action: &str) -> String;

    /// Queue the restores of the agents the removal stopped, warning when
    /// that fails, since `shoal restore` still restores them.
    async fn queue_agents(&mut self, ctx: &Context);
}

impl ReopenedExt for Reopened {
    fn message(&self, ctx: &Context, action: &str) -> String {
        let Self {
            workspace, removed, ..
        } = self;
        let files = if removed.checkpoint.commit != removed.checkpoint.head {
            "; unsaved files restored"
        } else if removed.checkpoint.files_omitted {
            "; unsaved files were not kept"
        } else {
            ""
        };
        let agents = match removed.agents.len() {
            _ if self.queued.is_some() => self
                .queued
                .as_ref()
                .and_then(Queued::summary)
                .map(|summary| format!("; {summary}"))
                .unwrap_or_default(),
            _ if !self.agents_saved => String::new(),
            0 => String::new(),
            1 => format!(
                "; shoal restore {} restores its stopped agent",
                workspace.number
            ),
            _ => format!(
                "; shoal restore {} restores its stopped agents",
                workspace.number
            ),
        };
        format!(
            "{action} {} {}{files}{agents}",
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            super::super::workspaces::location_phrase(workspace),
        )
    }

    async fn queue_agents(&mut self, ctx: &Context) {
        let executions: BTreeSet<ExecutionId> = self
            .removed
            .agents
            .iter()
            .map(|agent| agent.execution.clone())
            .collect();
        if executions.is_empty() {
            return;
        }
        let stopped = Stopped::from([(self.workspace.id.clone(), executions)]);
        match super::agents::queue::queue_stopped(ctx, &stopped).await {
            Ok(queued) => {
                self.queued = Some(queued);
                self.agents_saved = false;
            }
            Err(error) => eprintln!(
                "warning: cannot queue the stopped agents; shoal restore {} restores them: {error:#}",
                self.workspace.number
            ),
        }
    }
}

/// Print the reopening's warnings, trust its path for the agents that resume
/// there and put their session files in place, set the workspace up and
/// link its PRs again; `None` when its setup did not finish.
pub(in crate::cli::commands) async fn prepare_reopened(
    ctx: &Context,
    restored: RestoredWorkspace,
) -> Result<Option<Reopened>> {
    let RestoredWorkspace {
        opened,
        removed,
        session_files,
        warnings,
    } = restored;
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    crate::cli::agents::trust_resumed(ctx, &opened.workspace, &removed).await;
    crate::cli::agents::install_kept_session_files(
        ctx,
        &opened.workspace,
        &removed,
        &session_files,
    )
    .await;
    let mut workspace = opened.workspace;
    if workspace.state == WorkspaceState::Preparing {
        let Some(set_up) =
            super::super::workspaces::setup_workspace(ctx, &workspace, false).await?
        else {
            return Ok(None);
        };
        workspace = set_up.workspace;
    }
    for url in &removed.links.prs {
        workspace = crate::opened_pull::link(&ctx.paths, workspace, url.clone()).await?;
    }
    Ok(Some(Reopened {
        agents_saved: !(removed.agents.is_empty() && removed.last_sessions.is_empty()),
        workspace,
        removed,
        warnings,
        queued: None,
    }))
}

/// One aligned line per removed workspace: its name, branch, when and why
/// it was removed, and the number of the checkpoint its removal took.
pub(super) fn removed_lines(removed: &[RemovedWorkspace]) -> Vec<String> {
    let width = removed
        .iter()
        .map(|workspace| workspace.name.chars().count())
        .max()
        .unwrap_or(0);
    removed
        .iter()
        .map(|workspace| {
            let branch = branch_label(workspace.checkpoint.branch.as_ref());
            format!(
                "{:width$}  {branch}  removed {} ({}), checkpoint {}, restorable until {}",
                workspace.name,
                workspace.removed_at.local_seconds(),
                workspace.cause,
                workspace.checkpoint.number,
                workspace.expires_at.local_seconds(),
            )
        })
        .collect()
}
