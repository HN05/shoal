//! Placing the agent of the user's start without a terminal once its
//! workspace is set up, from that workspace's own settings: queued for the
//! daemon, or, for an agent that runs its own way and so leaves this
//! process at once, started here when the agent limits admit it. The
//! daemon carries out every other start without a terminal, and one whose
//! agent waits for other work.
use std::ffi::OsString;

use anyhow::Result;

use super::{
    AddOptions,
    blocked::start_blocked,
    finish::{AgentStart, add_planned, holding, report_queued_in},
    queue::launch_blockers,
    target::AddTarget,
};
use crate::{
    agent::Agent,
    cli::{
        agents,
        commands::{issues, queued_agents},
        context::Context,
        terminal,
    },
    model::{Blocker, QueueEntry, Workspace},
    protocol::{ConfigTarget, operations::QueuedLaunch},
    queued_launches,
    start_plan::{AddPlan, ResolvedAddAgent},
};
use shoal_protocol::space_names::SpaceSelector;

/// Whether `plan`'s agent is placed once its workspace is set up: the
/// user's, started without a terminal and not here, which the daemon leaves
/// to this process only when it runs its own way.
pub(super) fn places_after_setup(ctx: &Context, plan: &AddPlan, options: &AddOptions) -> bool {
    plan.agent_name().is_some()
        && !crate::env::is_scoped()
        && super::detached::leaves_caller(ctx, options.here)
}

/// Open the planned workspace and place its agent once setup ends. One
/// that waits for `blockers`, or the issues the forge records as blocking
/// its issue, the daemon records at once, with its agent queued there, as
/// from a terminal.
pub(super) async fn add_placed(
    ctx: &Context,
    plan: AddPlan,
    target: AddTarget,
    issue: Option<issues::Issue>,
    options: AddOptions,
    blockers: Vec<Blocker>,
) -> Result<i32> {
    let now = options.now;
    let blockers = launch_blockers(ctx, &target, issue.as_ref(), now, blockers).await?;
    if blockers.is_empty() {
        return add_planned(ctx, plan, target, issue, AgentStart::Placed { now }).await;
    }
    start_blocked(ctx, plan, &target, issue.as_ref(), blockers).await
}

/// The launch of the agent that starts in this process.
pub(super) struct HereLaunch {
    pub agent: ResolvedAddAgent,
    pub opening: agents::Opening,
    /// The permissions `--permission` gives the agent, whatever the mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
    pub args: Vec<OsString>,
    pub fork: Option<shoal_ids::agent_session_ids::AgentSessionId>,
}

impl HereLaunch {
    /// Start the agent in `workspace`.
    pub(super) async fn run(self, ctx: &Context, workspace: Workspace) -> Result<i32> {
        let Self {
            agent,
            opening,
            narrowing,
            args,
            fork,
        } = self;
        let launch = super::agent::Launch {
            opening,
            narrowing,
            args,
            fork,
        };
        super::agent::launch(ctx, agent, workspace, launch).await
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
            return queued_agents::report_queued(ctx, &entry, waiting);
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
    Here(Option<shoal_ids::queue_entry_ids::QueueEntryId>),
}

/// Place `launch` in the set-up `workspace`.
async fn place(ctx: &Context, workspace: &Workspace, launch: QueuedLaunch) -> Result<Placement> {
    let settings = shoal_workers::settings::of(
        &ctx.paths,
        ConfigTarget::Space(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let agent: Agent = launch.agent.name().parse()?;
    if agents::runs_its_own_way(&agent, &settings) {
        return admit_here(ctx, workspace, launch).await;
    }
    crate::terminal::queued::ensure_background(&settings)?;
    let entry = queued_launches::queue_in_workspace(&ctx.paths, workspace, launch, None).await?;
    Ok(Placement::Queued(Box::new(entry)))
}

/// Admit `launch`, which starts here, as a terminal's start is admitted: at
/// once when a slot is free or `now` says so, otherwise waiting for the
/// daemon to start it as the launch it plans there. Either way its entry
/// opens `workspace`, which the request names.
async fn admit_here(
    ctx: &Context,
    workspace: &Workspace,
    launch: QueuedLaunch,
) -> Result<Placement> {
    let request = queued_agents::Launch {
        agent: launch.agent.clone(),
        name: workspace.name.clone(),
        issue_title: launch.issue.as_ref().map(|issue| issue.title.clone()),
        issue_labels: (launch.issue.as_ref()).map_or_else(Vec::new, |issue| issue.labels.clone()),
        now: launch.now,
        opens: Some(workspace.id.clone()),
    };
    let plan = AddPlan::agent_in(workspace, launch)?;
    match queued_agents::admit(ctx, &plan, request).await? {
        queued_agents::Admission::Start { entry, .. } => {
            if let Some(entry) = &entry {
                queued_agents::attach(ctx, entry, workspace).await?;
            }
            Ok(Placement::Here(entry))
        }
        queued_agents::Admission::Queued(entry, waiting) => Ok(Placement::Waiting(entry, waiting)),
    }
}
