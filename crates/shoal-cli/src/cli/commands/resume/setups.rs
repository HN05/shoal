//! A workspace whose setup failed, or never started, resumes by retrying its
//! setup, after which the agent waiting in the queue for it starts as the
//! queue admits it, or at once for `--now`.
use anyhow::{Context as _, Result};

use super::targets::Selected;
use crate::client::output::{batch::Failure, resume::SetUp};
use crate::{
    cli::{
        commands::{queued_agents, workspaces},
        context::Context,
        ui,
    },
    model::{QueueEntry, Workspace},
    state::{QueueKind, QueueState},
};

/// A selected workspace whose setup failed or never started, with the
/// launch of the agent waiting for it, if one does.
#[derive(Debug)]
pub(super) struct FailedSetup {
    pub workspace: Workspace,
    pub launch: Option<QueueEntry>,
}

/// Move the selected workspaces whose setup failed or never started from
/// `selected`'s workspaces to its setups.
pub(super) async fn take_failed(ctx: &Context, selected: &mut Selected) -> Result<()> {
    let Some(selectors) = selected.workspaces.take() else {
        return Ok(());
    };
    let (mut failed, mut rest) = (Vec::new(), Vec::new());
    for selector in selectors {
        let inspection = crate::client::workspaces::inspect(&ctx.paths, selector.clone()).await?;
        match workspaces::awaits_setup(&inspection) {
            true => failed.push(inspection.workspace),
            false => rest.push(selector),
        }
    }
    selected.workspaces = Some(rest);
    selected.setups = waiting(ctx, failed).await?;
    Ok(())
}

/// The `workspaces` whose setup failed, each with the launch waiting for
/// it.
async fn waiting(ctx: &Context, workspaces: Vec<Workspace>) -> Result<Vec<FailedSetup>> {
    if workspaces.is_empty() {
        return Ok(Vec::new());
    }
    let entries = queued_agents::entries(ctx).await?;
    let waiting = workspaces.into_iter().map(|workspace| FailedSetup {
        launch: awaiting(&entries, &workspace).cloned(),
        workspace,
    });
    Ok(waiting.collect())
}

/// The launch among `entries` that waits for the setup of `workspace`.
fn awaiting<'a>(entries: &'a [QueueEntry], workspace: &Workspace) -> Option<&'a QueueEntry> {
    entries.iter().find(|entry| {
        entry.kind == QueueKind::Launch
            && entry.state == QueueState::Queued
            && entry.opens.as_ref() == Some(&workspace.id)
    })
}

/// Retry the setup of each of `setups`, starting the agent waiting for it
/// at once when `now`, going on after one fails, which it reports.
pub(super) async fn retry_each(
    ctx: &Context,
    setups: &[FailedSetup],
    now: bool,
) -> (Vec<SetUp>, Vec<Failure>) {
    let (mut set_up, mut failed) = (Vec::new(), Vec::new());
    for setup in setups {
        match retry(ctx, setup, now).await {
            Ok(done) => set_up.push(done),
            Err(error) => failed.push(ui::failure(ctx, "resume", &setup.workspace.name, &error)),
        }
    }
    (set_up, failed)
}

/// Retry `setup`, then start its waiting agent at once when `now`;
/// otherwise the queue starts it as it admits it.
async fn retry(ctx: &Context, setup: &FailedSetup, now: bool) -> Result<SetUp> {
    match setup.launch.as_ref().filter(|_| now) {
        Some(launch) => retry_starting(ctx, &setup.workspace, launch).await,
        None => retry_queued(ctx, setup).await,
    }
}

/// Retry `setup`, leaving the agent waiting for it, if any, to the queue.
async fn retry_queued(ctx: &Context, setup: &FailedSetup) -> Result<SetUp> {
    let workspace = set_up(ctx, &setup.workspace).await?;
    let launch = match &setup.launch {
        Some(launch) => current(ctx, launch).await?,
        None => None,
    };
    Ok(SetUp { workspace, launch })
}

/// Retry the setup of `workspace`, then start `launch`, the agent waiting
/// for it, at once, which the queue leaves to this process meanwhile.
async fn retry_starting(
    ctx: &Context,
    workspace: &Workspace,
    launch: &QueueEntry,
) -> Result<SetUp> {
    let _held = queued_agents::Hold::start(&ctx.paths, &launch.id).await;
    let workspace = set_up(ctx, workspace).await?;
    let launch = crate::client::queue::start(&ctx.paths, launch.id.to_string()).await?;
    Ok(SetUp {
        workspace,
        launch: Some(launch),
    })
}

/// Run `workspace`'s setup and its post-setup hook again.
async fn set_up(ctx: &Context, workspace: &Workspace) -> Result<Workspace> {
    let set_up = workspaces::setup_workspace(ctx, workspace, true).await?;
    let set_up =
        set_up.with_context(|| format!("the setup of {} did not complete", workspace.name))?;
    set_up.finish(ctx).await
}

/// `launch` as the queue has it now, gone once its agent started.
async fn current(ctx: &Context, launch: &QueueEntry) -> Result<Option<QueueEntry>> {
    let entries = queued_agents::entries(ctx).await?;
    Ok(entries.into_iter().find(|entry| entry.id == launch.id))
}
