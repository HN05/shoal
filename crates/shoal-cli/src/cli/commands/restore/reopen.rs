//! Opening removed spaces again from their removal records.
use crate::cli::commands::restore::agents::reports::QueuedExt;
use crate::client::execution_ids::ExecutionId;
use std::collections::BTreeSet;

use anyhow::{Context as _, Result};

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
    model::{PresentSpace, RemovedSpace, RestoredChat, RestoredSpace, RestoredWorkspace},
    state::WorkspaceState,
};

/// Open each of the `removed` spaces again, in the order given, so a
/// workspace opens before a chat in its worktree: link its PRs and set up a
/// workspace and run its post-setup hook, in which the agents restored for
/// it start; an error once one fails names those opened before it. A
/// workspace opened alone becomes the directory of a shell that follows
/// the caller.
pub(super) async fn reopen_each(ctx: &Context, removed: &[RemovedSpace]) -> Result<Vec<Reopened>> {
    let mut reopened = Vec::new();
    for space in removed {
        let opened = reopen(ctx, space).await;
        reopened.push(opened.map_err(|error| Reopening::of(&reopened).explain(error))?);
    }
    if let [opened] = reopened.as_slice()
        && let Some(workspace) = opened.space.worktree()
        && terminal::follows_caller(ctx)
    {
        shell::navigate(ctx, &workspace.path)?;
    }
    Ok(reopened)
}

/// Open the `removed` space again: a workspace set up, with its PRs linked
/// and its post-setup hook run, a chat with its PRs linked.
async fn reopen(ctx: &Context, removed: &RemovedSpace) -> Result<Reopened> {
    let restore = crate::client::removal::restore(&ctx.paths, SpaceSelector::id(&removed.space_id));
    let restored = ctx.progress("Restoring", restore).await?;
    let restored = match restored {
        RestoredSpace::Workspace(restored) => restored,
        RestoredSpace::Chat(restored) => return reopened_chat(ctx, *restored).await,
    };
    let number = restored.opened.workspace.number;
    let opened = (prepare_reopened(ctx, *restored).await?).with_context(|| {
        format!(
            "the setup of {} did not finish; shoal restore {number} retries it",
            removed.name
        )
    })?;
    if let Some(workspace) = opened.space.worktree() {
        super::super::workspaces::run_post_setup(ctx, workspace).await?;
    }
    Ok(opened)
}

/// Print the warnings of the chat `restored` and link its PRs again.
pub(in crate::cli::commands) async fn reopened_chat(
    ctx: &Context,
    restored: RestoredChat,
) -> Result<Reopened> {
    let RestoredChat {
        chat,
        removed,
        warnings,
    } = restored;
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    for pr in &removed.links.prs {
        let url = pr.url.clone().into();
        crate::opened_pull::link_or_warn(&ctx.paths, &chat.id, &url, pr.title.clone()).await;
    }
    Ok(Reopened {
        agents_saved: !(removed.agents.is_empty() && removed.last_sessions.is_empty()),
        space: PresentSpace::Chat(chat),
        removed,
        warnings,
        queued: None,
    })
}

/// The spaces a restore opened again, which an error that ends it
/// afterwards names, since it reports them no other way.
pub(super) struct Reopening(Option<String>);

impl Reopening {
    pub(super) fn of(reopened: &[Reopened]) -> Self {
        let names = reopened
            .iter()
            .map(|opened| format!("{} as {}", opened.space.name(), opened.space.number()))
            .collect::<Vec<_>>();
        Self((!names.is_empty()).then(|| names.join(", ")))
    }

    /// `error`, saying which spaces opened before it.
    pub(super) fn explain(&self, error: anyhow::Error) -> anyhow::Error {
        match &self.0 {
            Some(names) => error.context(format!("reopened {names}, then stopped")),
            None => error,
        }
    }
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
    let workspace = (reopened.space.worktree().cloned()).context("a workspace reopened")?;
    // The agents start in a workspace whose post-setup hook succeeded.
    if restore {
        super::super::workspaces::run_post_setup(ctx, &workspace).await?;
        reopened.queue_agents(ctx).await;
    }
    ctx.emit(&reopened.message(ctx, action), &reopened)?;
    if terminal::follows_caller(ctx) {
        shell::navigate(ctx, &workspace.path)?;
    }
    if !restore {
        super::super::workspaces::run_post_setup(ctx, &workspace).await?;
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
        let Self { space, removed, .. } = self;
        let checkpoint = removed.workspace().map(|kept| &kept.checkpoint);
        let files = if checkpoint.is_some_and(|checkpoint| checkpoint.commit != checkpoint.head) {
            "; unsaved files restored"
        } else if checkpoint.is_some_and(|checkpoint| checkpoint.files_omitted) {
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
                space.number()
            ),
            _ => format!(
                "; shoal restore {} restores its stopped agents",
                space.number()
            ),
        };
        let location = match space.worktree() {
            Some(workspace) => super::super::workspaces::location_phrase(workspace),
            None => format!("as chat {}", space.number()),
        };
        format!(
            "{action} {} {location}{files}{agents}",
            Palette::stdout(ctx.json).paint(Style::Heading, space.name()),
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
        let stopped = Stopped::from([(self.space.id().clone(), executions)]);
        match super::agents::queue::queue_stopped(ctx, &stopped).await {
            Ok(queued) => {
                self.queued = Some(queued);
                self.agents_saved = false;
            }
            Err(error) => eprintln!(
                "warning: cannot queue the stopped agents; shoal restore {} restores them: {error:#}",
                self.space.number()
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
    for pr in &removed.links.prs {
        workspace = crate::opened_pull::link(&ctx.paths, workspace, pr.clone()).await?;
    }
    Ok(Some(Reopened {
        agents_saved: !(removed.agents.is_empty() && removed.last_sessions.is_empty()),
        space: PresentSpace::Workspace(workspace),
        removed,
        warnings,
        queued: None,
    }))
}

/// One aligned line per removed space: its name, a workspace's branch, when
/// and why it was removed, and the number of the checkpoint a workspace's
/// removal took.
pub(super) fn removed_lines(removed: &[RemovedSpace]) -> Vec<String> {
    let width = removed
        .iter()
        .map(|space| space.name.chars().count())
        .max()
        .unwrap_or(0);
    removed
        .iter()
        .map(|space| {
            let (branch, checkpoint) = match space.workspace() {
                Some(kept) => (
                    format!("  {}", branch_label(kept.checkpoint.branch.as_ref())),
                    format!(", checkpoint {}", kept.checkpoint.number),
                ),
                None => Default::default(),
            };
            format!(
                "{:width$}{branch}  removed {} ({}){checkpoint}, restorable until {}",
                space.name,
                space.removed_at.local_seconds(),
                space.cause,
                space.expires_at.local_seconds(),
            )
        })
        .collect()
}
