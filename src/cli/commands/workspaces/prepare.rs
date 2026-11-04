//! Preparing a workspace inside an operation's worker: its tracked setup,
//! failing as the operation reports it, then linking its PR, then its
//! post-setup hook, failing in turn, then queuing its agent.

use anyhow::{Result, ensure};

use super::environment::configured_environment;
use super::link_pull::link_or_warn;
use super::{check_queued_launch, queued_agent_request};
use crate::{
    cli::{
        client::{self},
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
    state::WorkspaceState,
};

/// Run `steps` in `workspace`, returning it ready with the agent queued.
pub(in crate::cli::commands) async fn prepare(
    ctx: &Context,
    workspace: String,
    steps: Preparation,
    reporter: &Reporter<'_>,
) -> Result<Prepared> {
    let mut workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    if steps.setup {
        reporter.step(Step::SetupStarted).await?;
        workspace = run_setup(ctx, &workspace).await?;
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
/// Shoal could not verify keeps its own error.
async fn run_setup(ctx: &Context, workspace: &Workspace) -> Result<Workspace> {
    let failure = match execution::setup(&ctx.paths, workspace.id.clone()).await {
        Ok(0) => None,
        Ok(code) => Some(format!("setup command exited with status {code}")),
        Err(error) if error.is::<execution::SetupVerificationFailure>() => return Err(error),
        Err(error) => Some(format!("{error:#}")),
    };
    if let Some(message) = failure {
        return Err(Failed(Failure::SetupFailed { message }).into());
    }
    let workspace = client::inspect(&ctx.paths, workspace.id.clone())
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
    let command =
        crate::client::workspaces::hook(&ctx.paths, workspace.id.clone(), HookKind::PostSetup)
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
    Ok(client::inspect(&ctx.paths, workspace.id).await?.workspace)
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
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    check_queued_launch(&launch, &settings)?;
    let request = queued_agent_request(workspace, launch)?;
    reporter.ensure_running()?;
    let admission = crate::client::queue::add(&ctx.paths, request).await?;
    admission
        .entry
        .ok_or_else(|| anyhow::anyhow!("the daemon queued the agent without an entry"))
}
