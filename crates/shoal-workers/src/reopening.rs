//! Opening spaces again that the daemon restored from their removal
//! records. What the process tells its own way is its [`Reopening`]
//! frontend.
use std::collections::BTreeSet;

use anyhow::{Context as _, Result};
use shoal_core::transfer::CarriedRecord;
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transfer::StoppedExecutions;

use crate::{
    client::{connection::CommandPaths, output::restore::Reopened},
    model::{RemovedSpace, RestoredChat, RestoredSpace, RestoredWorkspace, Space},
    preparation::{self, Frontend},
    resume::Resuming,
    state::WorkspaceState,
};

/// What a process that opens spaces again does its own way, besides
/// preparing them as its [`Frontend`] does.
pub trait Reopening: Frontend {
    /// How the process tells of the stopped agents whose restores it queues.
    fn resuming(&self) -> impl Resuming + '_;

    /// Tell of `reopened`, a space a package carries, which its import
    /// opened again.
    fn carried(&self, reopened: &Reopened);
}

/// [`prepare_restored`], refusing a workspace whose preparation did not
/// finish with the command that retries it.
pub async fn prepare_set_up(
    paths: &CommandPaths,
    reopening: &impl Reopening,
    restored: RestoredSpace,
) -> Result<Reopened> {
    let (name, number) = match &restored {
        RestoredSpace::Workspace(restored) => {
            let workspace = &restored.opened.workspace;
            (workspace.name.clone(), workspace.number)
        }
        RestoredSpace::Chat(restored) => (restored.chat.name.clone(), restored.chat.number),
    };
    (prepare_restored(paths, reopening, restored).await?).with_context(|| {
        format!("the preparation of {name} did not finish; shoal setup {number} retries it")
    })
}

/// Prepare the space `restored` opened again: a workspace as
/// [`prepare_reopened`] does, a chat as [`reopened_chat`] does; `None` when
/// a workspace's worktree or setup did not finish.
pub async fn prepare_restored(
    paths: &CommandPaths,
    reopening: &impl Reopening,
    restored: RestoredSpace,
) -> Result<Option<Reopened>> {
    match restored {
        RestoredSpace::Workspace(restored) => prepare_reopened(paths, reopening, *restored).await,
        RestoredSpace::Chat(restored) => reopened_chat(paths, *restored).await.map(Some),
    }
}

/// Run the post-setup hook of the workspace `reopened` opened again; a chat
/// has none.
pub async fn run_post_setup(
    paths: &CommandPaths,
    reopening: &impl Reopening,
    reopened: &Reopened,
) -> Result<()> {
    match reopened.space.to_workspace() {
        Some(workspace) => preparation::run_post_setup(paths, reopening, &workspace).await,
        None => Ok(()),
    }
}

/// Print the warnings of the chat `restored`, put the session files kept
/// for its agent in place and link its PRs again.
async fn reopened_chat(paths: &CommandPaths, restored: RestoredChat) -> Result<Reopened> {
    let RestoredChat {
        chat,
        removed,
        session_files,
        warnings,
    } = restored;
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    let home = crate::launch::SessionHome::Chat(&chat);
    crate::launch::install_kept_session_files(paths, home, &removed, &session_files).await;
    for pr in &removed.links.prs {
        let url = pr.url.clone().into();
        crate::opened_pull::link_or_warn(paths, &chat.id, &url, pr.title.clone()).await;
    }
    Ok(Reopened {
        agents_saved: !(removed.agents.is_empty() && removed.last_sessions.is_empty()),
        space: Space::from(chat),
        removed,
        warnings,
        queued: None,
    })
}

/// Print the reopening's warnings, trust its path for the agents that resume
/// there and put their session files in place, set the workspace up and
/// link its PRs again; `None` when its worktree or setup did not finish.
async fn prepare_reopened(
    paths: &CommandPaths,
    reopening: &impl Reopening,
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
    crate::launch::trust_resumed(paths, &opened.workspace, &removed).await;
    let home = crate::launch::SessionHome::Workspace(&opened.workspace);
    crate::launch::install_kept_session_files(paths, home, &removed, &session_files).await;
    let mut workspace = opened.workspace;
    if workspace.state == WorkspaceState::Preparing {
        let set_up = preparation::setup_workspace(paths, reopening, &workspace, false).await?;
        let Some(set_up) = set_up else {
            return Ok(None);
        };
        workspace = set_up.workspace;
    }
    for pr in &removed.links.prs {
        workspace = crate::opened_pull::link(paths, workspace, pr.clone()).await?;
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

/// Queue the restores of the agents the removal of `reopened` stopped and
/// of the last sessions it kept, warning when that fails, since `shoal
/// resume` still resumes them.
pub async fn queue_agents(
    paths: &CommandPaths,
    reopening: &impl Reopening,
    reopened: &mut Reopened,
) {
    let executions = saved_executions(&reopened.removed);
    if executions.is_empty() {
        return;
    }
    let stopped = StoppedExecutions::from([(reopened.space.id.clone(), executions)]);
    let resuming = reopening.resuming();
    match crate::resume::stopped::restore(paths, &stopped, &resuming).await {
        Ok(queued) => {
            reopened.queued = Some(queued);
            reopened.agents_saved = false;
        }
        Err(error) => eprintln!(
            "warning: cannot queue the stopped agents; shoal resume {} resumes them: {error:#}",
            reopened.space.number
        ),
    }
}

/// The executions of the stopped agents and last sessions `removed` keeps.
pub fn saved_executions(removed: &RemovedSpace) -> BTreeSet<ExecutionId> {
    let agents = removed.agents.iter().map(|agent| &agent.execution);
    let last = removed.last_sessions.iter().map(|last| &last.execution);
    agents.chain(last).cloned().collect()
}

/// Open each space the import `carried` recorded again, in order, parents
/// first, as `shoal restore` does, queueing their agents' restores when
/// `restore`; the failure of each space that did not open, whose record,
/// when the import made one, stays for `shoal restore`.
pub async fn open_carried(
    paths: &CommandPaths,
    reopening: &impl Reopening,
    carried: Vec<CarriedRecord>,
    restore: bool,
) -> Vec<anyhow::Error> {
    let mut failed = Vec::new();
    for record in carried {
        match record {
            CarriedRecord::Recorded {
                name,
                space,
                warnings,
            } => {
                for warning in &warnings {
                    eprintln!("warning: {warning}");
                }
                if let Err(error) = open_one(paths, reopening, &space, restore).await {
                    let retry = format!("cannot open {name} again; shoal restore {space} retries");
                    failed.push(error.context(retry));
                }
            }
            CarriedRecord::Failed { name, error } => {
                failed.push(anyhow::anyhow!("cannot open {name} again: {error}"));
            }
        }
    }
    failed
}

/// Refuse an import whose carried spaces `failed` to open, so a move keeps
/// what did not arrive.
pub fn ensure_carried_opened(failed: Vec<anyhow::Error>) -> Result<()> {
    if failed.is_empty() {
        return Ok(());
    }
    let failures: Vec<String> = failed.iter().map(|error| format!("{error:#}")).collect();
    anyhow::bail!(
        "the space opened, but not every space it carries:\n  {}",
        failures.join("\n  ")
    )
}

/// Open the recorded space `id` again as [`open_carried`] does, and tell
/// of it.
async fn open_one(
    paths: &CommandPaths,
    reopening: &impl Reopening,
    id: &SpaceId,
    restore: bool,
) -> Result<()> {
    let restored = crate::client::removal::restore(paths, SpaceSelector::id(id)).await?;
    let mut reopened = prepare_set_up(paths, reopening, restored).await?;
    let post_setup = run_post_setup(paths, reopening, &reopened).await;
    if post_setup.is_ok() && restore {
        queue_agents(paths, reopening, &mut reopened).await;
    }
    reopening.carried(&reopened);
    post_setup.context("it opened, but its hooks.post_setup failed")
}
