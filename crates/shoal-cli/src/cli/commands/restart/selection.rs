//! The spaces a run restarts the agents of, as the command selects them.
use anyhow::{Result, ensure};

use crate::client::restarts::{Selection, running_agents};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{args::SpaceTargets, context::Context, ui},
    model::{PresentSpace, Spaces},
};

/// `--all` selects every workspace and chat but the caller's own, names
/// select theirs, and neither the picker. The daemon lists only the agents
/// a scoped caller may restart.
pub(super) async fn load(ctx: &Context, targets: SpaceTargets) -> Result<Selection> {
    let named = !targets.all && !targets.space.is_empty();
    let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
    let spaces = match targets.all {
        true => crate::cli::scoping::reached_below(spaces),
        false => {
            let selected = ui::select_spaces(ctx, targets.space, false).await?;
            in_order(ctx, &spaces, selected).await?
        }
    };
    let agents = running_agents(&ctx.paths).await?;
    Ok(Selection {
        spaces,
        agents,
        named,
    })
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
                .all(|space| Some(space.id()) != own.as_ref()),
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
