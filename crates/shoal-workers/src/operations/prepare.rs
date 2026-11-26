//! Preparing a workspace.
use anyhow::{Result, ensure};

use super::{Failed, Reporter};
use crate::{
    client::connection::CommandPaths,
    execution,
    forge::items::reference::ItemInput,
    hooks::{self, Hook, HookKind},
    model::{QueueEntry, Workspace},
    protocol::operations::{Failure, Preparation, Prepared, QueuedLaunch, Step},
    queued_launches,
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;

/// Run `steps` in `workspace`, returning it ready with the agent queued.
pub(super) async fn prepare(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    steps: Preparation,
    reporter: &Reporter<'_>,
) -> Result<Prepared> {
    let mut workspace = crate::client::workspaces::inspect(workspace)
        .send(paths)
        .await?
        .workspace;
    let for_agent = steps.agent.is_some() || steps.waiting.is_some();
    let readied = ready(paths, workspace.clone(), (steps.setup, for_agent), reporter);
    workspace = match readied.await {
        Ok(readied) => readied,
        Err(error) => {
            // The workspace stays for a retried preparation, with the agent
            // that waits for it and links its PR once it starts.
            if let Some(launch) = steps.agent.or(steps.waiting) {
                leave_waiting(paths, &workspace, *launch, steps.pr).await;
            }
            return Err(error);
        }
    };
    if let Some(pr) = steps.pr {
        workspace = link_pr(paths, workspace, &pr, reporter).await?;
    }
    // A workspace created without setup runs none of its hooks.
    if steps.post_setup && !workspace.setup_skipped {
        reporter.step(Step::PostSetupStarted).await?;
        run_post_setup_hook(paths, &workspace, reporter)
            .await
            .map_err(|error| {
                Failed(Failure::HookFailed {
                    message: format!("{error:#}"),
                })
            })?;
    }
    // The agents waiting for the workspace's setup may start now, once this
    // preparation got past its post-setup hook.
    if steps.post_setup && workspace.state == WorkspaceState::Ready {
        reporter.reached(Step::PostSetupEnded).await?;
    }
    let agent = match steps.agent {
        Some(launch) => {
            let entry = queue_agent(paths, &workspace, *launch, reporter).await?;
            reporter.reached(Step::AgentQueued).await?;
            Some(entry)
        }
        None => None,
    };
    let workspace = Box::new(workspace);
    Ok(Prepared { workspace, agent })
}

/// Create the worktree `workspace` has yet to get. When `setup`, run its
/// setup, for the agent that starts there when `for_agent`, unless the new
/// worktree needs none. Returns the workspace as it leaves it.
async fn ready(
    paths: &CommandPaths,
    workspace: Workspace,
    (setup, for_agent): (bool, bool),
    reporter: &Reporter<'_>,
) -> Result<Workspace> {
    let pending = workspace.checkout.is_pending();
    let workspace = match pending {
        true => create_worktree(paths, &workspace, reporter).await?,
        false => workspace,
    };
    if !setup {
        return Ok(workspace);
    }
    // A new worktree without setup to run is ready already.
    let workspace = match pending && workspace.state == WorkspaceState::Ready {
        true => workspace,
        false => {
            reporter.step(Step::SetupStarted).await?;
            set_up(paths, &workspace, for_agent).await?
        }
    };
    reporter.step(Step::Ready).await?;
    Ok(workspace)
}

/// Create the worktree `workspace` has yet to get, failing as the operation
/// reports it.
async fn create_worktree(
    paths: &CommandPaths,
    workspace: &Workspace,
    reporter: &Reporter<'_>,
) -> Result<Workspace> {
    reporter.step(Step::WorktreeStarted).await?;
    let selector = SpaceSelector::id(&workspace.id);
    (crate::client::workspaces::create_pending_worktree(paths, selector).await).map_err(|error| {
        let message = format!("{error:#}");
        Failed(Failure::WorktreeFailed { message }).into()
    })
}

/// Run the setup command, then start the containers that start with setup,
/// for the agent that starts in the workspace when `for_agent`.
async fn set_up(paths: &CommandPaths, workspace: &Workspace, for_agent: bool) -> Result<Workspace> {
    let workspace = run_setup(paths, workspace).await?;
    let selector = SpaceSelector::id(&workspace.id);
    if let Err(error) = crate::client::leases::set_up_containers(paths, selector, for_agent).await {
        let message = format!("{error:#}");
        return Err(Failed(Failure::SetupFailed { message }).into());
    }
    Ok(workspace)
}

/// Run the setup command as a tracked execution; a setup whose processes
/// Shoal could not verify, or that never started, keeps its own error.
async fn run_setup(paths: &CommandPaths, workspace: &Workspace) -> Result<Workspace> {
    let failure = match execution::setup(paths, SpaceSelector::Id(workspace.id.clone())).await {
        Ok(0) => None,
        Ok(code) => Some(format!("setup command exited with status {code}")),
        Err(error) if error.is::<execution::SetupVerificationFailure>() => return Err(error),
        Err(error) if error.is::<execution::SetupNotStarted>() => {
            // Why it did not start, below the context that says so.
            let causes: Vec<_> = error.chain().skip(1).map(ToString::to_string).collect();
            let message = causes.join(": ");
            return Err(Failed(Failure::SetupNotStarted { message }).into());
        }
        Err(error) => Some(format!("{error:#}")),
    };
    if let Some(message) = failure {
        return Err(Failed(Failure::SetupFailed { message }).into());
    }
    let workspace = crate::client::workspaces::inspect(SpaceSelector::id(&workspace.id))
        .send(paths)
        .await?
        .workspace;
    ensure!(
        workspace.state == WorkspaceState::Ready,
        "workspace setup did not complete"
    );
    Ok(workspace)
}

/// Run the configured `hooks.post_setup`, if any, until the operation is
/// cancelled.
async fn run_post_setup_hook(
    paths: &CommandPaths,
    workspace: &Workspace,
    reporter: &Reporter<'_>,
) -> Result<()> {
    let command = crate::client::workspaces::hook(
        paths,
        SpaceSelector::id(&workspace.id),
        HookKind::PostSetup,
    )
    .await?;
    if let Some(command) = command {
        let space = crate::model::Space::from(workspace.clone());
        let configured_env = crate::settings::configured_environment(paths, &space).await?;
        hooks::run_until(
            Hook::PostSetup,
            workspace,
            &command,
            paths,
            &configured_env,
            reporter.cancelled(),
        )
        .await?;
    }
    Ok(())
}

/// Link `pr`, a number or URL, as the PR the ready workspace works on,
/// unless the operation was cancelled, and return the workspace with it. The
/// workspace is kept either way, so a refused link only warns.
async fn link_pr(
    paths: &CommandPaths,
    workspace: Workspace,
    pr: &ItemInput,
    reporter: &Reporter<'_>,
) -> Result<Workspace> {
    reporter.ensure_running()?;
    if !crate::opened_pull::link_or_warn(paths, &workspace.id, pr, None).await {
        return Ok(workspace);
    }
    reporter.reached(Step::PrLinked).await?;
    Ok(
        crate::client::workspaces::inspect(SpaceSelector::id(&workspace.id))
            .send(paths)
            .await?
            .workspace,
    )
}

/// Queue `launch` in the ready workspace, which the agent queue starts
/// detached in the default host. A cancel that arrives first queues
/// nothing; the queue may start the agent as it admits it, so once queued
/// it stays.
async fn queue_agent(
    paths: &CommandPaths,
    workspace: &Workspace,
    launch: QueuedLaunch,
    reporter: &Reporter<'_>,
) -> Result<QueueEntry> {
    reporter.ensure_running()?;
    queued_launches::queue_in_workspace(paths, workspace, launch, None).await
}

/// Leave `launch` waiting in the queue of `workspace`, whose setup failed,
/// to start once a retried setup readies it, linking `pr` then, and say so;
/// a refusal only warns.
async fn leave_waiting(
    paths: &CommandPaths,
    workspace: &Workspace,
    launch: QueuedLaunch,
    pr: Option<ItemInput>,
) {
    let queued = queued_launches::queue_for_setup(paths, workspace, launch, pr);
    let (name, number) = (&workspace.name, workspace.number);
    match queued.await {
        Ok(entry) => eprintln!(
            "{} waits for the setup of {name}; `shoal setup {number}` retries it",
            entry.agent
        ),
        Err(error) => {
            eprintln!("warning: the agent of {name} cannot wait for its setup: {error:#}")
        }
    }
}
