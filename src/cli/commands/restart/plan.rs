//! Which agents a run restarts, in what order, and which it skips: those of
//! workspaces and of chats alike.
use anyhow::{Result, ensure};

pub(super) use crate::client::output::agents::Skipped;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{args::SpaceTargets, context::Context, ui},
    model::{PendingRestart, PresentSpace, RunningAgent, Spaces, Workspace},
    state::AgentState,
};

/// The selected spaces and the agents that run there.
pub(super) struct Selection {
    spaces: Vec<PresentSpace>,
    agents: Vec<RunningAgent>,
    /// The spaces were named, so one without an agent is reported.
    named: bool,
}

/// What a run restarts, in order, and what it skips.
pub(super) struct Plan {
    pub(super) restarts: Vec<Restart>,
    pub(super) skipped: Vec<Skipped>,
    /// Names the run to the daemon, which records it on each restart.
    pub(super) run: String,
}

/// One agent to restart.
pub(super) struct Restart {
    /// The workspace or chat it runs in.
    pub(super) space: PresentSpace,
    pub(super) agent: RunningAgent,
    /// The restart in progress when the run selected it, which the run
    /// waits for instead of starting another, even once it ends.
    pub(super) joins: Option<PendingRestart>,
}

impl Restart {
    /// Whether the agent can restart without interrupting a turn: it
    /// reported itself that its turn ended, since another agent's report in
    /// the same workspace does not count, or the run joins its restart.
    pub(super) fn ready(&self) -> bool {
        self.agent.state == Some(AgentState::Idle) || self.joins.is_some()
    }

    /// The agent, skipped for `reason`.
    pub(super) fn skipped(self, reason: impl Into<String>) -> Skipped {
        Skipped {
            space: self.space.name().to_string(),
            agent: self.agent.agent,
            reason: reason.into(),
        }
    }
}

impl Selection {
    /// `--all` selects every workspace and chat, names select theirs, and
    /// neither the picker.
    pub(super) async fn load(ctx: &Context, targets: SpaceTargets) -> Result<Self> {
        let named = !targets.all && !targets.space.is_empty();
        let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
        let spaces = match targets.all {
            true => every(spaces),
            false => {
                let selected = ui::select_spaces(ctx, targets.space, false).await?;
                in_order(ctx, &spaces, selected).await?
            }
        };
        let agents = running_agents(ctx).await?;
        Ok(Self {
            spaces,
            agents,
            named,
        })
    }

    /// Refuse a selection that includes the scoped caller's own space,
    /// before any restarts, as the daemon would refuse its agent.
    pub(super) fn ensure_below(&self) -> Result<()> {
        let own = crate::env::own_space();
        ensure!(
            own.is_some()
                && self
                    .spaces
                    .iter()
                    .all(|space| Some(space.id()) != own.as_ref()),
            "scoped processes restart only the spaces below their own; finish their own with shoal done"
        );
        Ok(())
    }

    /// The agents named `agent` to restart, ready ones first, and those that
    /// cannot restart, `in_place` when asked, or already run again. Agents
    /// protection stopped wait for their restore and are left out, and
    /// reported as skipped when their space was named and runs no other.
    pub(super) fn plan(self, agent: Option<&crate::agent::Agent>, in_place: bool) -> Plan {
        let mut plan = Plan {
            restarts: Vec::new(),
            skipped: Vec::new(),
            run: uuid::Uuid::new_v4().to_string(),
        };
        for space in self.spaces {
            // An execution whose wrapper is gone may run any agent.
            let (stopped, agents): (Vec<_>, Vec<_>) = self
                .agents
                .iter()
                .filter(|running| running.space_id == *space.id())
                .filter(|running| {
                    agent.is_none() || running.agent.is_none() || running.agent.as_ref() == agent
                })
                .cloned()
                .partition(|running| running.stopped.is_some());
            if agents.is_empty() && self.named {
                plan.skipped.push(Skipped {
                    space: space.name().to_string(),
                    agent: agent.cloned(),
                    reason: absent_reason(space.name(), agent, &stopped),
                });
            }
            for running in agents {
                match run_skip_reason(&space, &running, in_place) {
                    Some(reason) => plan.skipped.push(Skipped {
                        space: space.name().to_string(),
                        agent: running.agent,
                        reason,
                    }),
                    None => plan.restarts.push(Restart {
                        space: space.clone(),
                        joins: running.restart.clone(),
                        agent: running,
                    }),
                }
            }
        }
        plan.continue_earlier_run();
        plan.restarts.sort_by_key(|restart| !restart.ready());
        plan
    }
}

impl Plan {
    /// Restarts in progress come from an earlier run, which this one
    /// continues under its name: an agent that run restarted already runs
    /// again.
    fn continue_earlier_run(&mut self) {
        let runs: Vec<String> = self
            .restarts
            .iter()
            .filter_map(|restart| restart.agent.restart.as_ref())
            .flat_map(|pending| pending.run.iter().chain(&pending.joined).cloned())
            .collect();
        let Some(run) = runs.first() else {
            return;
        };
        self.run.clone_from(run);
        let (again, restarts) =
            std::mem::take(&mut self.restarts)
                .into_iter()
                .partition(|restart: &Restart| {
                    restart.agent.restart.is_none()
                        && restart
                            .agent
                            .restarted_by
                            .iter()
                            .any(|run| runs.contains(run))
                });
        self.restarts = restarts;
        self.skipped.extend(
            again
                .into_iter()
                .map(|restart| restart.skipped("the interrupted run restarted it already")),
        );
    }
}

/// The agents of workspaces and of chats, as a run selects and follows them.
pub(super) async fn running_agents(ctx: &Context) -> Result<Vec<RunningAgent>> {
    let mut agents = crate::client::agents::all(&ctx.paths).await?;
    agents.extend(crate::client::agents::of_chats(&ctx.paths).await?);
    Ok(agents)
}

/// Every workspace and chat of `spaces`.
fn every(spaces: Spaces) -> Vec<PresentSpace> {
    let workspaces = spaces.workspaces.into_iter().map(PresentSpace::Workspace);
    let chats = spaces.chats.into_iter().map(PresentSpace::Chat);
    workspaces.chain(chats).collect()
}

/// The spaces `selected` names among `spaces`, in its order, each once. A
/// selector no listed space has is inspected, which names why the daemon
/// does not know it.
async fn in_order(
    ctx: &Context,
    spaces: &Spaces,
    selected: Vec<SpaceSelector>,
) -> Result<Vec<PresentSpace>> {
    let mut ordered: Vec<PresentSpace> = Vec::new();
    for selector in selected {
        let space = match spaces.find(&selector) {
            Some(space) => space,
            None => {
                let inspection =
                    crate::client::workspaces::inspect(&ctx.paths, selector.clone()).await?;
                PresentSpace::Workspace(inspection.workspace)
            }
        };
        if ordered.iter().all(|listed| listed.id() != space.id()) {
            ordered.push(space);
        }
    }
    Ok(ordered)
}

/// Why a run that restarts `in_place` when asked cannot restart `running`,
/// the agent of `space`, if it cannot.
fn run_skip_reason(space: &PresentSpace, running: &RunningAgent, in_place: bool) -> Option<String> {
    match space {
        PresentSpace::Chat(_) if in_place => {
            Some("a chat restarts only in a new place; omit --in-place".into())
        }
        space => space_skip_reason(space, running),
    }
}

/// Why `running`, the agent of `space`, cannot restart, if it cannot.
pub(super) fn space_skip_reason(space: &PresentSpace, running: &RunningAgent) -> Option<String> {
    match space {
        PresentSpace::Workspace(workspace) => skip_reason(workspace, running),
        PresentSpace::Chat(_) if !running.restorable => {
            Some("its agent cannot resume its session".into())
        }
        PresentSpace::Chat(_) => None,
    }
}

/// Why the named `space` restarts no agent named `agent`, when the only ones
/// it has are those of `stopped`, which protection stopped.
fn absent_reason(
    space: &str,
    agent: Option<&crate::agent::Agent>,
    stopped: &[RunningAgent],
) -> String {
    if let Some(reason) = stopped
        .iter()
        .find_map(|running| stopped_reason(space, running))
    {
        return reason;
    }
    match agent {
        Some(agent) => format!("no {agent} agent runs there"),
        None => "no agent runs there".into(),
    }
}

/// Why `running`, the agent of `space`, does not restart when protection
/// stopped it: it waits for its restore, which `shoal queue start` starts
/// now.
pub(super) fn stopped_reason(space: &str, running: &RunningAgent) -> Option<String> {
    let protection = running.stopped.as_ref()?;
    let agent = running
        .agent
        .as_ref()
        .map(|agent| agent.name())
        .unwrap_or("its agent");
    Some(format!(
        "{agent} is stopped ({}) and resumes once {}; restore it now with shoal queue start {space}",
        protection.reason, protection.resumes_when
    ))
}

/// Why `running` cannot restart in place, if it cannot; `shoal queue move`
/// refuses such an agent too.
pub(in crate::cli::commands) fn skip_reason(
    workspace: &Workspace,
    running: &RunningAgent,
) -> Option<String> {
    let manual = format!("use shoal stop {0} and shoal resume {0}", workspace.name);
    match &running.agent {
        None => Some(format!(
            "execution {} is disconnected; {manual}",
            running.execution
        )),
        Some(agent) if !running.restorable => Some(format!(
            "{agent} has no command to resume its session; {manual}"
        )),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests;
