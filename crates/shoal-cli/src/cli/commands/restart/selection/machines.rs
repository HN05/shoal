//! The spaces a restart selects on other joined machines, from the hub's
//! directory: every space of the machines `--machine` names, or one a
//! selector names that this machine has none of.
use anyhow::{Context as _, Result, anyhow};
use shoal_protocol::{
    hub::{
        Member,
        directory::{HubView, Listed},
    },
    space_names::SpaceSelector,
};

use super::{Chosen, reached};
use crate::cli::{commands::machines, context::Context};

/// Every space of `named`, joined machines by their name or ID: this
/// machine's as `--all` selects them, and another's as the hub lists them.
pub(super) async fn on(ctx: &Context, named: &[String]) -> Result<Chosen> {
    let view = crate::client::hub::directory(&ctx.paths).await?.context(
        "this machine joined no hub, so it runs every space it has; leave out --machine",
    )?;
    let mut chosen = Chosen::default();
    for machine in distinct(&view, named)? {
        match machine.id == view.machine {
            true => {
                let spaces = crate::client::spaces::list(&ctx.paths).await?;
                chosen.here = reached(ctx, spaces);
            }
            false => chosen.elsewhere.extend(spaces_of(&view, &machine)),
        }
    }
    Ok(chosen)
}

/// The machines `named` names in `view`, each once.
fn distinct(view: &HubView, named: &[String]) -> Result<Vec<Member>> {
    let mut machines: Vec<Member> = Vec::new();
    for name in named {
        let machine = (view.directory.named_machine(name)).map_err(|message| anyhow!(message))?;
        if machines.iter().all(|known| known.id != machine.id) {
            machines.push(machine.clone());
        }
    }
    Ok(machines)
}

/// The spaces `view` lists on `machine`, each with it.
fn spaces_of(view: &HubView, machine: &Member) -> Vec<(Listed, Member)> {
    (view.directory.spaces.iter())
        .filter(|listed| listed.machine == machine.id)
        .map(|listed| (listed.clone(), machine.clone()))
        .collect()
}

/// The other machine's space `selector` names when this machine has none,
/// with its machine.
pub(super) async fn named_elsewhere(
    ctx: &Context,
    selector: &SpaceSelector,
) -> Result<Option<(Listed, Member)>> {
    let Some((listed, view)) = machines::space_elsewhere(ctx, selector).await? else {
        return Ok(None);
    };
    let machine = (view.directory.machine(&listed.machine))
        .with_context(|| format!("the hub lists space {selector} on a machine it does not list"))?;
    Ok(Some((listed.clone(), machine.clone())))
}

#[cfg(test)]
mod tests;
