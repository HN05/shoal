//! Opening removed workspaces again from their removal records.
use crate::cli::commands::restore::agents::reports::QueuedExt;
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use std::collections::BTreeSet;

use anyhow::Result;

use super::agents::queue::Stopped;
use super::checkpoints::branch_label;
use crate::client::output::resume::{Queued, Reopened};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        shell, terminal, ui,
    },
    model::{RemovedWorkspace, RestoredWorkspace},
    state::WorkspaceState,
};

/// Reopen the removed workspace the user picks, or list the removed
/// workspaces when there is no terminal or none to pick.
pub(super) async fn choose_removed(ctx: &Context) -> Result<i32> {
    let removed = crate::client::removal::removed(&ctx.paths).await?;
    if removed.is_empty() || !ctx.interactive() {
        ctx.show(&removed, |removed| print_removed(removed))?;
        return Ok(0);
    }
    let entries = removed
        .iter()
        .map(|workspace| workspace.workspace_id.to_string())
        .zip(removed_lines(&removed))
        .collect();
    let workspace = SpaceId::from(ui::pick(ctx, "Removed workspace> ", entries)?);
    reopen(ctx, SpaceSelector::Id(workspace)).await
}

/// Open a removed workspace again from its removal record.
pub(super) async fn reopen(ctx: &Context, workspace: SpaceSelector) -> Result<i32> {
    let restored = ctx
        .progress(
            "Restoring workspace",
            crate::client::removal::restore(&ctx.paths, workspace),
        )
        .await?;
    finish_reopen(ctx, *restored, "Restored", false).await
}

/// Set up a workspace the daemon opened again from a removal record, and
/// link its PRs, then queue its stopped agents when `resume`; `action`
/// begins the report.
pub(in crate::cli::commands) async fn finish_reopen(
    ctx: &Context,
    restored: RestoredWorkspace,
    action: &str,
    resume: bool,
) -> Result<i32> {
    let Some(mut reopened) = prepare_reopened(ctx, restored).await? else {
        return Ok(1);
    };
    // The agents start in a workspace whose post-setup hook succeeded.
    if resume {
        super::super::workspaces::run_post_setup(ctx, &reopened.workspace).await?;
        reopened.queue_agents(ctx).await;
    }
    ctx.emit(&reopened.message(ctx, action), &reopened)?;
    if terminal::follows_caller(ctx) {
        shell::navigate(ctx, &reopened.workspace.path)?;
    }
    if !resume {
        super::super::workspaces::run_post_setup(ctx, &reopened.workspace).await?;
    }
    Ok(0)
}

/// Operations on the client type [`Reopened`] that only the `shoal` command uses.
pub(crate) trait ReopenedExt {
    /// The report of the reopening, which `action` begins.
    fn message(&self, ctx: &Context, action: &str) -> String;

    /// Queue the restores of the agents the removal stopped, warning when
    /// that fails, since `shoal resume` still restores them.
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
            0 => String::new(),
            1 => format!(
                "; shoal resume {} continues its stopped agent",
                workspace.number
            ),
            _ => format!(
                "; shoal resume {} continues its stopped agents",
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
            Ok(queued) => self.queued = Some(queued),
            Err(error) => eprintln!(
                "warning: cannot queue the stopped agents; shoal resume {} restores them: {error:#}",
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
        workspace,
        removed,
        warnings,
        queued: None,
    }))
}

/// List `removed`, each led by the ID that selects it, since a removed
/// workspace has no number and its name selects nothing.
fn print_removed(removed: &[RemovedWorkspace]) {
    if removed.is_empty() {
        outln!("No removed workspaces to restore");
    }
    for (workspace, line) in removed.iter().zip(removed_lines(removed)) {
        outln!("{}  {line}", workspace.workspace_id);
    }
}

/// One aligned line per removed workspace: its name, branch, when and why
/// it was removed, and the number of the checkpoint its removal took.
fn removed_lines(removed: &[RemovedWorkspace]) -> Vec<String> {
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
