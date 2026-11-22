//! Opening removed spaces again from their removal records.
use crate::cli::commands::resume::reports::QueuedExt;
use shoal_ids::execution_ids::ExecutionId;
use shoal_workers::hosting;
use std::collections::BTreeSet;

use anyhow::{Context as _, Result};

use crate::cli::commands::resume::queue::Stopped;
use crate::client::output::{restore::Reopened, resume::Queued};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        shell,
    },
    model::{Head, RemovedSpace, RestoredChat, RestoredSpace, RestoredWorkspace, Space},
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;

/// Open each of the `removed` spaces again, in the order given, so a
/// workspace opens before a chat in its worktree: link its PRs and set up a
/// workspace and run its post-setup hook, in which the agents restored for
/// it start; an error once one fails names those opened before it. A
/// workspace opened alone becomes the directory of a shell that follows
/// the caller.
pub(in crate::cli::commands) async fn reopen_each(
    ctx: &Context,
    removed: &[RemovedSpace],
) -> Result<Vec<Reopened>> {
    let mut reopened = Vec::new();
    for space in removed {
        let opened = reopen(ctx, space).await;
        reopened.push(opened.map_err(|error| Reopening::of(&reopened).explain(error))?);
    }
    if let [opened] = reopened.as_slice()
        && let Some(workspace) = opened.space.worktree()
        && hosting::follows_caller(ctx.caller())
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
    let opened = prepare_set_up(ctx, restored).await?;
    run_post_setup(ctx, &opened).await?;
    Ok(opened)
}

/// [`prepare_restored`], refusing a workspace whose preparation did not
/// finish with the command that retries it.
pub(in crate::cli::commands) async fn prepare_set_up(
    ctx: &Context,
    restored: RestoredSpace,
) -> Result<Reopened> {
    let (name, number) = match &restored {
        RestoredSpace::Workspace(restored) => {
            let workspace = &restored.opened.workspace;
            (workspace.name.clone(), workspace.number)
        }
        RestoredSpace::Chat(restored) => (restored.chat.name.clone(), restored.chat.number),
    };
    (prepare_restored(ctx, restored).await?).with_context(|| {
        format!("the preparation of {name} did not finish; shoal setup {number} retries it")
    })
}

/// Prepare the space `restored` opened again: a workspace as
/// [`prepare_reopened`] does, a chat as [`reopened_chat`] does; `None` when
/// a workspace's worktree or setup did not finish.
async fn prepare_restored(ctx: &Context, restored: RestoredSpace) -> Result<Option<Reopened>> {
    match restored {
        RestoredSpace::Workspace(restored) => prepare_reopened(ctx, *restored).await,
        RestoredSpace::Chat(restored) => reopened_chat(ctx, *restored).await.map(Some),
    }
}

/// Run the post-setup hook of the workspace `reopened` opened again; a chat
/// has none.
pub(in crate::cli::commands) async fn run_post_setup(
    ctx: &Context,
    reopened: &Reopened,
) -> Result<()> {
    match reopened.space.to_workspace() {
        Some(workspace) => super::super::setup::run_post_setup(ctx, &workspace).await,
        None => Ok(()),
    }
}

/// Print the warnings of the chat `restored`, put the session files kept
/// for its agent in place and link its PRs again.
async fn reopened_chat(ctx: &Context, restored: RestoredChat) -> Result<Reopened> {
    let RestoredChat {
        chat,
        removed,
        session_files,
        warnings,
    } = restored;
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    let home = crate::cli::agents::SessionHome::Chat(&chat);
    crate::cli::agents::install_kept_session_files(ctx, home, &removed, &session_files).await;
    for pr in &removed.links.prs {
        let url = pr.url.clone().into();
        crate::opened_pull::link_or_warn(&ctx.paths, &chat.id, &url, pr.title.clone()).await;
    }
    Ok(Reopened {
        agents_saved: !(removed.agents.is_empty() && removed.last_sessions.is_empty()),
        space: Space::from(chat),
        removed,
        warnings,
        queued: None,
    })
}

/// The spaces a restore opened again, which an error that ends it
/// afterwards names, since it reports them no other way.
pub(in crate::cli::commands) struct Reopening(Option<String>);

impl Reopening {
    pub(in crate::cli::commands) fn of(reopened: &[Reopened]) -> Self {
        let names = reopened
            .iter()
            .map(|opened| format!("{} as {}", opened.space.name, opened.space.number))
            .collect::<Vec<_>>();
        Self((!names.is_empty()).then(|| names.join(", ")))
    }

    /// `error`, saying which spaces opened before it.
    pub(in crate::cli::commands) fn explain(&self, error: anyhow::Error) -> anyhow::Error {
        match &self.0 {
            Some(names) => error.context(format!("reopened {names}, then stopped")),
            None => error,
        }
    }
}

/// Prepare a space the daemon opened again from a removal record, as
/// [`prepare_restored`] does, then queue its stopped agents when `restore`;
/// `action` begins the report. A workspace becomes the directory of a shell
/// that follows the caller.
pub(in crate::cli::commands) async fn finish_reopen(
    ctx: &Context,
    restored: RestoredSpace,
    action: &str,
    restore: bool,
) -> Result<i32> {
    let Some(mut reopened) = prepare_restored(ctx, restored).await? else {
        return Ok(1);
    };
    // The agents start in a workspace whose post-setup hook succeeded.
    if restore {
        run_post_setup(ctx, &reopened).await?;
        reopened.queue_agents(ctx).await;
    }
    ctx.emit(&reopened.message(ctx, action), &reopened)?;
    if let Some(workspace) = reopened.space.worktree()
        && hosting::follows_caller(ctx.caller())
    {
        shell::navigate(ctx, &workspace.path)?;
    }
    if !restore {
        run_post_setup(ctx, &reopened).await?;
    }
    Ok(0)
}

/// Operations on the client type [`Reopened`] that only the `shoal` command uses.
pub(crate) trait ReopenedExt {
    /// The report of the reopening, which `action` begins.
    fn message(&self, ctx: &Context, action: &str) -> String;

    /// Queue the restores of the agents the removal stopped and of the
    /// last sessions it kept, warning when that fails, since `shoal
    /// resume` still resumes them.
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
        let agents = match saved_executions(removed).len() {
            _ if self.queued.is_some() => self
                .queued
                .as_ref()
                .and_then(Queued::summary)
                .map(|summary| format!("; {summary}"))
                .unwrap_or_default(),
            _ if !self.agents_saved => String::new(),
            0 => String::new(),
            1 => format!("; shoal resume {} resumes its stopped agent", space.number),
            _ => format!("; shoal resume {} resumes its stopped agents", space.number),
        };
        let location = match space.to_workspace() {
            Some(workspace) => super::super::preparation::location_phrase(&workspace),
            None => format!("as chat {}", space.number),
        };
        format!(
            "{action} {} {location}{files}{agents}",
            Palette::stdout(ctx.json).paint(Style::Heading, &space.name),
        )
    }

    async fn queue_agents(&mut self, ctx: &Context) {
        let executions = saved_executions(&self.removed);
        if executions.is_empty() {
            return;
        }
        let stopped = Stopped::from([(self.space.id.clone(), executions)]);
        match crate::cli::commands::resume::queue::queue_stopped(ctx, &stopped).await {
            Ok(queued) => {
                self.queued = Some(queued);
                self.agents_saved = false;
            }
            Err(error) => eprintln!(
                "warning: cannot queue the stopped agents; shoal resume {} resumes them: {error:#}",
                self.space.number
            ),
        }
    }
}

/// The executions of the stopped agents and last sessions `removed` keeps.
fn saved_executions(removed: &RemovedSpace) -> BTreeSet<ExecutionId> {
    let agents = removed.agents.iter().map(|agent| &agent.execution);
    let last = removed.last_sessions.iter().map(|last| &last.execution);
    agents.chain(last).cloned().collect()
}

/// Print the reopening's warnings, trust its path for the agents that resume
/// there and put their session files in place, set the workspace up and
/// link its PRs again; `None` when its worktree or setup did not finish.
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
    let home = crate::cli::agents::SessionHome::Workspace(&opened.workspace);
    crate::cli::agents::install_kept_session_files(ctx, home, &removed, &session_files).await;
    let mut workspace = opened.workspace;
    if workspace.state == WorkspaceState::Preparing {
        let Some(set_up) = super::super::setup::setup_workspace(ctx, &workspace, false).await?
        else {
            return Ok(None);
        };
        workspace = set_up.workspace;
    }
    for pr in &removed.links.prs {
        workspace = crate::opened_pull::link(&ctx.paths, workspace, pr.clone()).await?;
    }
    // Its worktree failed, as a warning said; its preparation retries it.
    if workspace.checkout.is_pending() {
        return Ok(None);
    }
    Ok(Some(Reopened {
        agents_saved: !(removed.agents.is_empty() && removed.last_sessions.is_empty()),
        space: Space::from(workspace),
        removed,
        warnings,
        queued: None,
    }))
}

/// One aligned line per removed space: its name, the branch or commit a
/// workspace's restore opens, when and why it was removed, and the number of the checkpoint a workspace's
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
                    match &kept.head {
                        Head::Branch(branch) => format!("  {branch}"),
                        Head::Commit(at) => format!("  at {}", at.label()),
                    },
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
