//! The spaces a command restarts the agents of, as it selects them, on this
//! machine and on the other joined machines.
use std::collections::HashMap;

use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::hub::{Member, directory::Listed};

use super::target::Target;
use crate::client::output::agents::Skipped;
use crate::{
    agent::Agent,
    cli::{args::SpaceTargets, context::Context, ui},
    model::{RunningAgent, Space, Spaces},
    sessions::Origin,
    time::UnixTime,
};
use shoal_protocol::space_names::SpaceSelector;

mod machines;

/// The spaces whose agents a command asks the daemon to restart, and those
/// it leaves out.
pub(super) struct Selection {
    pub spaces: Vec<Target>,
    pub skipped: Vec<Skipped>,
    /// Where the agent of each selected space of this machine runs, when
    /// its wrapper runs in a terminal host.
    pub places: HashMap<SpaceId, Origin>,
}

/// The spaces a command names or selects, before their agents filter them:
/// this machine's, and other machines' with the machine of each.
#[derive(Default)]
pub(super) struct Chosen {
    pub here: Vec<Space>,
    pub elsewhere: Vec<(Listed, Member)>,
}

/// `--all` selects every workspace and chat of this machine but the
/// caller's own, `--machine` every space of the machines it names, names
/// select theirs wherever they run, and none of them the picker, with
/// `agent` only the spaces whose agent is named so. The daemon lists only
/// the agents a scoped caller may restart, and the hub only the spaces of
/// other machines its scope reaches.
pub(super) async fn load(
    ctx: &Context,
    targets: SpaceTargets,
    machines: &[String],
    agent: Option<&Agent>,
) -> Result<Selection> {
    let named = !targets.all && !targets.space.is_empty();
    let chosen = spaces(ctx, targets, machines).await?;
    let agents = crate::client::agents::all(&ctx.paths).await?;
    let mut selection = select(chosen.here, &agents, agent, named);
    select_elsewhere(&mut selection, chosen.elsewhere, agent, named);
    Ok(selection)
}

/// The spaces `targets` or `machines` select: with `--machine` every space
/// of those machines, with `--all` every workspace and chat of this
/// machine but the caller's own, else those its names select, or the
/// picker.
pub(super) async fn spaces(
    ctx: &Context,
    targets: SpaceTargets,
    machines: &[String],
) -> Result<Chosen> {
    if !machines.is_empty() {
        return machines::on(ctx, machines).await;
    }
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    match targets.all {
        true => Ok(Chosen {
            here: reached(ctx, spaces),
            elsewhere: Vec::new(),
        }),
        false => {
            let selected = ui::select_spaces(ctx, targets.space, false).await?;
            in_order(ctx, &spaces, selected).await
        }
    }
}

/// The spaces of `spaces` that `--all` selects: every one the caller's
/// scope reaches but its own.
fn reached(ctx: &Context, spaces: Spaces) -> Vec<Space> {
    crate::cli::scoping::Scoping::inherited(&ctx.paths.state).reached_below(spaces)
}

/// The spaces of `spaces` whose agent among `agents` is named `agent`, and
/// without names each that runs one Shoal did not stop. The daemon tells
/// why it skips a named one; one that runs no `agent` agent is skipped here.
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

/// Add to `selection` the spaces of other machines among `elsewhere` whose
/// agent their machine last reported named `agent`, and without names each
/// that runs one; a space of an offline machine that it would select is
/// skipped, naming the machine, unless named, which its machine's refusal
/// names.
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

/// The agent of `agents` a restart of `space` restarts, as the daemon picks
/// it: one Shoal did not stop before one it did, and a connected one before
/// an execution whose wrapper is gone.
fn restarted<'a>(agents: &'a [RunningAgent], space: &Space) -> Option<&'a RunningAgent> {
    let theirs = || (agents.iter()).filter(|running| running.space_id == space.id);
    let live = || theirs().filter(|running| running.stopped.is_none());
    (live().find(|running| running.agent.is_some()))
        .or_else(|| live().next())
        .or_else(|| theirs().next())
}

/// Refuse a selection that includes the scoped caller's own space,
/// before any restarts, as the daemon would refuse its agent. The daemon
/// refuses each space outside the caller's scope, which needs the scope
/// token: a process that cleared it stays refused.
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

/// The spaces `selected` names, in its order among those of each machine,
/// each once: among `spaces`, else among the other machines' the hub
/// lists. A selector no space has is inspected, which names why the daemon
/// does not know it.
async fn in_order(ctx: &Context, spaces: &Spaces, selected: Vec<SpaceSelector>) -> Result<Chosen> {
    let mut chosen = Chosen::default();
    for selector in selected {
        if let Some(space) = spaces.find(&selector) {
            chosen.add_here(space);
        } else if let Some(found) = machines::named_elsewhere(ctx, &selector).await? {
            chosen.add_elsewhere(found);
        } else {
            let inspection = crate::client::workspaces::inspect(&ctx.paths, selector).await?;
            chosen.add_here(inspection.workspace.into());
        }
    }
    Ok(chosen)
}

impl Chosen {
    /// The chosen spaces as targets, this machine's first.
    pub(super) fn into_targets(self) -> Vec<Target> {
        let elsewhere = (self.elsewhere.into_iter())
            .map(|(listed, machine)| Target::elsewhere(&listed, machine));
        (self.here.into_iter().map(Target::from))
            .chain(elsewhere)
            .collect()
    }

    /// Add `space` of this machine, unless chosen already.
    fn add_here(&mut self, space: Space) {
        if self.here.iter().all(|chosen| chosen.id != space.id) {
            self.here.push(space);
        }
    }

    /// Add another machine's space `listed`, unless chosen already.
    fn add_elsewhere(&mut self, (listed, machine): (Listed, Member)) {
        let known = (self.elsewhere.iter()).any(|(chosen, _)| chosen.space.id == listed.space.id);
        if !known {
            self.elsewhere.push((listed, machine));
        }
    }
}

#[cfg(test)]
mod tests;
