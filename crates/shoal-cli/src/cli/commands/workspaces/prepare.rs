//! Preparing a workspace inside an operation's worker: its tracked setup,
//! failing as the operation reports it, then linking its PR, then its
//! post-setup hook, failing in turn, then queuing its agent.

use anyhow::{Result, ensure};

use super::environment::configured_environment;
use super::link_pull::link_or_warn;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        commands::operations::{Failed, Reporter},
        context::Context,
    },
    execution,
    hooks::{self, Hook, HookKind},
    model::{QueueEntry, Workspace},
    protocol::{
        ConfigTarget,
        operations::{Failure, Preparation, Prepared, QueuedLaunch, Step},
    },
    queued_launches,
    state::WorkspaceState,
};

/// Run `steps` in `workspace`, returning it ready with the agent queued.
pub(in crate::cli::commands) async fn prepare(
    ctx: &Context,
    workspace: SpaceSelector,
    steps: Preparation,
    reporter: &Reporter<'_>,
) -> Result<Prepared> {
    let mut workspace = crate::client::workspaces::inspect(&ctx.paths, workspace)
        .await?
        .workspace;
    if steps.setup {
        reporter.step(Step::SetupStarted).await?;
        workspace = match run_setup(ctx, &workspace).await {
            Ok(set_up) => set_up,
            Err(error) => {
                // The workspace stays for a retried setup, with the agent
                // that waits for it and links its PR once it starts.
                if let Some(launch) = steps.agent {
                    leave_waiting(ctx, &workspace, *launch, steps.pr).await;
                }
                return Err(error);
            }
        };
        reporter.step(Step::Ready).await?;
    }
    if let Some(pr) = steps.pr {
        workspace = link_pr(ctx, workspace, &pr, reporter).await?;
    }
    // A workspace created without setup runs none of its hooks.
    if steps.post_setup && !workspace.setup_skipped {
        reporter.step(Step::PostSetupStarted).await?;
        run_post_setup_hook(ctx, &workspace, reporter)
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
            let entry = queue_agent(ctx, &workspace, *launch, reporter).await?;
            reporter.reached(Step::AgentQueued).await?;
            Some(entry)
        }
        None => None,
    };
    let workspace = Box::new(workspace);
    Ok(Prepared { workspace, agent })
}

/// Run the setup command as a tracked execution; a setup whose processes
/// Shoal could not verify, or that never started, keeps its own error.
async fn run_setup(ctx: &Context, workspace: &Workspace) -> Result<Workspace> {
    let failure = match execution::setup(&ctx.paths, SpaceSelector::Id(workspace.id.clone())).await
    {
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
    let workspace =
        crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace.id))
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
    ctx: &Context,
    workspace: &Workspace,
    reporter: &Reporter<'_>,
) -> Result<()> {
    let command = crate::client::workspaces::hook(
        &ctx.paths,
        SpaceSelector::id(&workspace.id),
        HookKind::PostSetup,
    )
    .await?;
    if let Some(command) = command {
        let configured_env = configured_environment(ctx, workspace).await?;
        hooks::run_until(
            Hook::PostSetup,
            workspace,
            &command,
            &ctx.paths,
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
    ctx: &Context,
    workspace: Workspace,
    pr: &str,
    reporter: &Reporter<'_>,
) -> Result<Workspace> {
    reporter.ensure_running()?;
    if !link_or_warn(ctx, &workspace.id, pr).await {
        return Ok(workspace);
    }
    reporter.reached(Step::PrLinked).await?;
    Ok(
        crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace.id))
            .await?
            .workspace,
    )
}

/// Queue `launch` in the ready workspace, which the agent queue starts
/// detached in the default host. A cancel that arrives first queues
/// nothing; the queue may start the agent as it admits it, so once queued
/// it stays.
async fn queue_agent(
    ctx: &Context,
    workspace: &Workspace,
    launch: QueuedLaunch,
    reporter: &Reporter<'_>,
) -> Result<QueueEntry> {
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    reporter.ensure_running()?;
    queued_launches::queue_in_workspace(&ctx.paths, workspace, &settings, launch, None).await
}

/// Leave `launch` waiting in the queue of `workspace`, whose setup failed,
/// to start once a retried setup readies it, linking `pr` then, and say so;
/// a refusal only warns.
pub(super) async fn leave_waiting(
    ctx: &Context,
    workspace: &Workspace,
    launch: QueuedLaunch,
    pr: Option<String>,
) {
    let queued = queued_launches::queue_for_setup(&ctx.paths, workspace, launch, pr);
    let (name, number) = (&workspace.name, workspace.number);
    match queued.await {
        Ok(entry) => eprintln!(
            "{} waits for the setup of {name}; `shoal resume {number}` retries it",
            entry.agent
        ),
        Err(error) => {
            eprintln!("warning: the agent of {name} cannot wait for its setup: {error:#}")
        }
    }
}
