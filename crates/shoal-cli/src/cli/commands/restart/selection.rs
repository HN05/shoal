//! The spaces a restart selects, on this machine and the other joined machines.
use std::collections::HashMap;

use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::hub::{Member, directory::Listed};

use super::target::Target;
use crate::client::output::agents::Skipped;
use crate::{
    agent::Agent,
    cli::{
        args::SpaceTargets,
        commands::space_selection::{self, Chosen},
        context::Context,
        ui::Acting,
    },
    model::{RunningAgent, Space, Spaces},
    sessions::Origin,
    time::UnixTime,
};

pub(super) struct Selection {
    pub spaces: Vec<Target>,
    pub skipped: Vec<Skipped>,
    /// Where the agent of each selected space of this machine runs, when
    /// its wrapper runs in a terminal host.
    pub places: HashMap<SpaceId, Origin>,
}

/// The daemon lists only the agents a scoped caller may restart, and the hub
/// only the spaces of other machines its scope reaches.
pub(super) async fn load(
    ctx: &Context,
    targets: SpaceTargets,
    agent: Option<&Agent>,
    acting: &Acting,
) -> Result<Selection> {
    let named = !targets.space.is_empty();
    let chosen = spaces(ctx, targets, acting).await?;
    let agents = crate::client::agents::all(&ctx.paths).await?;
    let mut selection = select(chosen.here, &agents, agent, named);
    select_elsewhere(&mut selection, chosen.elsewhere, agent, named);
    Ok(selection)
}

pub(super) async fn spaces(
    ctx: &Context,
    targets: SpaceTargets,
    acting: &Acting,
) -> Result<Chosen> {
    if targets.all {
        let spaces = crate::client::spaces::list(&ctx.paths).await?;
        return Ok(Chosen {
            here: reached(ctx, spaces),
            elsewhere: Vec::new(),
        });
    }
    let located = space_selection::located(ctx, targets, false, acting).await?;
    Ok(Chosen::of(located))
}

/// The spaces of `spaces` that `--all` selects: every one the caller's
/// scope reaches but its own.
fn reached(ctx: &Context, spaces: Spaces) -> Vec<Space> {
    crate::scoping::Scoping::inherited(&ctx.paths.state).reached_below(spaces)
}

/// The daemon tells why it skips a named space; one that runs no `agent` agent
/// is skipped here.
fn select(
    spaces: Vec<Space>,
    agents: &[RunningAgent],
    agent: Option<&Agent>,
    named: bool,
) -> Selection {
    let mut selection = Selection {
        spaces: Vec::new(),
        skipped: Vec::new(),
        places: HashMap::new(),
    };
    for space in spaces {
        // An execution whose wrapper is gone may run any agent.
        let theirs = restarted(agents, &space).filter(|running| {
            agent.is_none() || running.agent.is_none() || running.agent.as_ref() == agent
        });
        match (theirs, agent) {
            (Some(running), _) if named || running.stopped.is_none() => {
                if let Some(origin) = &running.origin {
                    selection.places.insert(space.id.clone(), origin.clone());
                }
                selection.spaces.push(space.into());
            }
            (None, None) if named => selection.spaces.push(space.into()),
            (None, Some(agent)) if named => selection.skipped.push(Skipped {
                space: space.name.clone(),
                agent: Some(agent.clone()),
                machine: None,
                reason: format!("no {agent} agent runs there"),
            }),
            _ => {}
        }
    }
    selection
}

/// A space of an offline machine that it would select is skipped, naming the
/// machine, unless named, which its machine's refusal names.
fn select_elsewhere(
    selection: &mut Selection,
    elsewhere: Vec<(Listed, Member)>,
    agent: Option<&Agent>,
    named: bool,
) {
    let now = UnixTime::now();
    for (listed, machine) in elsewhere {
        let running = (listed.space.running.as_ref())
            .filter(|running| agent.is_none_or(|agent| *running == agent));
        let target = Target::elsewhere(&listed, machine.clone());
        let skipped = |reason| Skipped {
            space: target.name.clone(),
            agent: running.or(agent).cloned(),
            machine: target.machine_name(),
            reason,
        };
        match (running, agent) {
            (Some(_), _) | (None, None) if named => selection.spaces.push(target),
            (Some(_), _) if machine.connected => selection.spaces.push(target),
            (Some(_), _) => selection.skipped.push(skipped(machine.offline(now))),
            (None, Some(agent)) if named => {
                let reason = format!("no {agent} agent runs there");
                selection.skipped.push(skipped(reason));
            }
            _ => {}
        }
    }
}

/// As the daemon picks it: one Shoal did not stop before one it did, and a
/// connected one before an execution whose wrapper is gone.
fn restarted<'a>(agents: &'a [RunningAgent], space: &Space) -> Option<&'a RunningAgent> {
    let theirs = || (agents.iter()).filter(|running| running.space_id == space.id);
    let live = || theirs().filter(|running| running.stopped.is_none());
    (live().find(|running| running.agent.is_some()))
        .or_else(|| live().next())
        .or_else(|| theirs().next())
}

/// Refused before any restart, as the daemon would refuse it. The daemon
/// refuses each space outside the caller's scope by its scope token, so a
/// process that cleared it stays refused.
pub(super) fn ensure_below(spaces: &[Target]) -> Result<()> {
    let own = crate::env::own_space();
    ensure!(
        own.is_some()
            && crate::env::is_scoped()
            && spaces.iter().all(|space| Some(&space.id) != own.as_ref()),
        "scoped processes restart only the spaces below their own; finish their own with shoal done"
    );
    Ok(())
}

pub(super) fn into_targets(chosen: Chosen) -> Vec<Target> {
    let elsewhere =
        (chosen.elsewhere.into_iter()).map(|(listed, machine)| Target::elsewhere(&listed, machine));
    (chosen.here.into_iter().map(Target::from))
        .chain(elsewhere)
        .collect()
}

#[cfg(test)]
mod tests;
