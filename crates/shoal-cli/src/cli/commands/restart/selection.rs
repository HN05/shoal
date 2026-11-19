//! The spaces a command restarts the agents of, as it selects them.
use std::collections::HashMap;

use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;

use crate::client::output::agents::Skipped;
use crate::{
    agent::Agent,
    cli::{args::SpaceTargets, context::Context, ui},
    model::{RunningAgent, Space, Spaces},
    sessions::Origin,
};
use shoal_protocol::space_names::SpaceSelector;

/// The spaces whose agents a command asks the daemon to restart, and those
/// it leaves out.
pub(super) struct Selection {
    pub spaces: Vec<Space>,
    pub skipped: Vec<Skipped>,
    /// Where the agent of each selected space runs, when its wrapper runs
    /// in a terminal host.
    pub places: HashMap<SpaceId, Origin>,
}

/// `--all` selects every workspace and chat but the caller's own, names
/// select theirs, and neither the picker, with `agent` only the spaces whose
/// agent is named so. The daemon lists only the agents a scoped caller may
/// restart.
pub(super) async fn load(
    ctx: &Context,
    targets: SpaceTargets,
    agent: Option<&Agent>,
) -> Result<Selection> {
    let named = !targets.all && !targets.space.is_empty();
    let spaces = spaces(ctx, targets).await?;
    let agents = crate::client::agents::all(&ctx.paths).await?;
    Ok(select(spaces, &agents, agent, named))
}

/// The spaces `targets` selects: with `--all` every workspace and chat but
/// the caller's own, else those its names select, or the picker.
pub(super) async fn spaces(ctx: &Context, targets: SpaceTargets) -> Result<Vec<Space>> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    match targets.all {
        true => Ok(crate::cli::scoping::Scoping::inherited(&ctx.paths.state).reached_below(spaces)),
        false => {
            let selected = ui::select_spaces(ctx, targets.space, false).await?;
            in_order(ctx, &spaces, selected).await
        }
    }
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
                selection.spaces.push(space);
            }
            (None, None) if named => selection.spaces.push(space),
            (None, Some(agent)) if named => selection.skipped.push(Skipped {
                space: space.name.clone(),
                agent: Some(agent.clone()),
                reason: format!("no {agent} agent runs there"),
            }),
            _ => {}
        }
    }
    selection
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
pub(super) fn ensure_below(selection: &Selection) -> Result<()> {
    let own = crate::env::own_space();
    ensure!(
        own.is_some()
            && crate::env::is_scoped()
            && selection
                .spaces
                .iter()
                .all(|space| Some(&space.id) != own.as_ref()),
        "scoped processes restart only the spaces below their own; finish their own with shoal done"
    );
    Ok(())
}

/// The spaces `selected` names among `spaces`, in its order, each once. A
/// selector no listed space has is inspected, which names why the daemon
/// does not know it.
async fn in_order(
    ctx: &Context,
    spaces: &Spaces,
    selected: Vec<SpaceSelector>,
) -> Result<Vec<Space>> {
    let mut ordered: Vec<Space> = Vec::new();
    for selector in selected {
        let space = match spaces.find(&selector) {
            Some(space) => space,
            None => {
                let inspection =
                    crate::client::workspaces::inspect(&ctx.paths, selector.clone()).await?;
                inspection.workspace.into()
            }
        };
        if ordered.iter().all(|listed| listed.id != space.id) {
            ordered.push(space);
        }
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests;
