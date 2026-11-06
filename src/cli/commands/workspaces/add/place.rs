//! Placing a noninteractive start's agent once its workspace is set up,
//! from that workspace's own settings: queued for the daemon, or, for an
//! agent that runs its own way and so leaves this process at once, started
//! here when the agent limits admit it.
use std::ffi::OsString;

use anyhow::Result;

use super::{
    agent::ResolvedAddAgent,
    finish::{holding, report_queued_in},
    plan::AddPlan,
};
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

/// The launch of the agent that starts in this process.
pub(super) struct HereLaunch {
    pub agent: ResolvedAddAgent,
    pub opening: agents::Opening,
    pub args: Vec<OsString>,
    pub fork: Option<String>,
}

impl HereLaunch {
    /// Start the agent in `workspace`.
    pub(super) async fn run(self, ctx: &Context, workspace: Workspace) -> Result<i32> {
        (self.agent)
            .launch(ctx, workspace, self.opening, self.args, self.fork)
            .await
    }
}

/// Place `launch` in the set-up `workspace` and report it, or start `here`
/// when it goes here, in this place, labelled `label`, under the queue
/// entry that holds its slot.
pub(super) async fn launch_placed(
    ctx: &Context,
    workspace: Workspace,
    launch: QueuedLaunch,
    here: HereLaunch,
    label: &str,
) -> Result<i32> {
    let slot = match place(ctx, &workspace, launch).await? {
        Placement::Queued(entry) => return report_queued_in(ctx, &workspace, &entry),
        Placement::Waiting(entry, waiting) => {
            return agent_queue::report_queued(ctx, &entry, waiting);
        }
        Placement::Here(slot) => slot,
    };
    terminal::host_workspace(ctx, &workspace, true, label).await?;
    let Some(entry) = slot else {
        return here.run(ctx, workspace).await;
    };
    crate::env::launch_queue_entry(&entry);
    holding(ctx, &entry, Box::pin(here.run(ctx, workspace))).await
}

/// Where a placed agent starts.
enum Placement {
    /// Queued in its workspace, which the daemon starts detached.
    Queued(Box<QueueEntry>),
    /// Waiting for a slot behind this many entries in total; the daemon
    /// starts it in the background once admitted.
    Waiting(Box<QueueEntry>, usize),
    /// Here, holding the queue entry that tracks its slot, if any.
    Here(Option<String>),
}

/// Place `launch` in the set-up `workspace`.
async fn place(ctx: &Context, workspace: &Workspace, launch: QueuedLaunch) -> Result<Placement> {
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
