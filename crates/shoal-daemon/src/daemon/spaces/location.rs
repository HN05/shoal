//! Where a space a request names runs: here, or on another joined machine
//! of the hub, among the spaces the caller sees.
use anyhow::{Context as _, Result};
use shoal_protocol::{
    hub::{Machine, directory::HubView},
    model::{Located, Space},
    space_names::SpaceSelector,
};

use crate::{
    daemon::{manager::Manager, scope::Caller},
    refusal::{Reason, Refusal},
};

impl Manager {
    /// Where the space `selector` names runs, among those `caller` sees:
    /// here, with its record, else on the other joined machine the hub
    /// lists it on; refused when neither has it.
    pub(crate) async fn locate_space(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
    ) -> Result<Located> {
        if let Some(space) = self.seen_here(caller, selector).await? {
            return Ok(Located::Here(Box::new(space)));
        }
        let view = (self.hub.hub_view(self, caller).await)
            .with_context(|| format!("no space {selector} here, and the hub cannot be asked"))?;
        if let Some(view) = &view
            && let Some(located) = listed_in(view, selector)?
        {
            return Ok(located);
        }
        Err(Refusal::new(Reason::NotFound, format!("no space {selector}")).into())
    }

    /// The space of this machine `selector` names, among those `caller`
    /// sees.
    async fn seen_here(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
    ) -> Result<Option<Space>> {
        let spaces = self.visible_spaces(caller).await?;
        Ok((spaces.into_iter()).find(|space| selector.selects(&space.id, space.number)))
    }
}

/// The other machine's space `selector` names in `view`, with that
/// machine, named by its ID where the directory lists no such machine.
fn listed_in(view: &HubView, selector: &SpaceSelector) -> Result<Option<Located>> {
    let Some(listed) = view.selected(selector).map_err(anyhow::Error::msg)? else {
        return Ok(None);
    };
    let machine = Machine {
        id: listed.machine.clone(),
        name: view.machine_name(listed).to_owned(),
    };
    Ok(Some(Located::Elsewhere {
        space: Box::new(listed.clone()),
        machine,
    }))
}

#[cfg(test)]
mod tests;
