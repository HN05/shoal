//! Placing a noninteractive start's agent once its workspace is set up,
//! from that workspace's own settings: queued for the daemon, or, for an
//! agent that runs its own way and so leaves this process at once, started
//! here when the agent limits admit it.
use anyhow::Result;

use super::plan::AddPlan;
use crate::{
    agent::Agent,
    cli::{
        agents,
        commands::{agent_queue, workspaces::prepare::queue_in_workspace},
        context::Context,
        terminal,
    },
    model::{QueueEntry, Workspace},
    protocol::{ConfigTarget, operations::QueuedLaunch},
};

/// Where a placed agent starts.
pub(super) enum Placement {
    /// Queued in its workspace, which the daemon starts detached.
    Queued(Box<QueueEntry>),
    /// Waiting for a slot behind this many entries in total; the daemon
    /// starts it in the background once admitted.
    Waiting(Box<QueueEntry>, usize),
    /// Here, holding the queue entry that tracks its slot, if any.
    Here(Option<String>),
}

/// Place `launch` in the set-up `workspace`.
pub(super) async fn place(
    ctx: &Context,
    workspace: &Workspace,
    launch: QueuedLaunch,
) -> Result<Placement> {
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(workspace.id.clone()),
    )
    .await?;
    let agent: Agent = launch.agent.parse()?;
    if agents::runs_its_own_way(&agent, &settings) {
        return admit_here(ctx, workspace, launch).await;
    }
    terminal::ensure_background(&settings)?;
    let entry = queue_in_workspace(ctx, workspace, &settings, launch).await?;
    Ok(Placement::Queued(Box::new(entry)))
}

/// Admit `launch`, which starts here, as a terminal's start is admitted: at
/// once when a slot is free or `now` says so, otherwise waiting for the
/// daemon to start it as the launch it plans there. Either way its entry
/// records `workspace`.
async fn admit_here(
    ctx: &Context,
    workspace: &Workspace,
    launch: QueuedLaunch,
) -> Result<Placement> {
    let request = agent_queue::Launch {
        agent: launch.agent.clone(),
        name: workspace.name.clone(),
        issue_title: launch.issue.as_ref().map(|issue| issue.title.clone()),
        now: launch.now,
        after: Vec::new(),
    };
    let plan = AddPlan::agent_in(workspace, launch)?;
    match agent_queue::admit(ctx, &plan, request).await? {
        agent_queue::Admission::Start { entry, .. } => {
            if let Some(entry) = &entry {
                agent_queue::attach(ctx, entry, workspace).await?;
            }
            Ok(Placement::Here(entry))
        }
        agent_queue::Admission::Queued(entry, waiting) => {
            agent_queue::attach(ctx, &entry.id, workspace).await?;
            Ok(Placement::Waiting(entry, waiting))
        }
    }
}
