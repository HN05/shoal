//! Where the spaces a request names run, and the spaces below them: here,
//! or on another joined machine of the hub, among the spaces the caller
//! sees.
use std::collections::{HashSet, VecDeque};

use anyhow::{Context as _, Result};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{
        Machine,
        directory::{HubView, Listed},
    },
    model::{Located, Space},
    space_names::SpaceSelector,
};

use crate::{
    daemon::{log, manager::Manager, scope::Caller},
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
            && let Some(listed) = view.selected(selector).map_err(anyhow::Error::msg)?
        {
            return Ok(located_elsewhere(view, listed));
        }
        Err(Refusal::new(Reason::NotFound, format!("no space {selector}")).into())
    }

    /// The spaces `selectors` name, located as [`Self::locate_space`]
    /// locates one, with the spaces below each on every machine when
    /// `below`, each once and after the space above it; a hub that cannot
    /// be asked leaves out the other machines' spaces below.
    pub(crate) async fn locate_tree(
        &self,
        caller: Option<&Caller>,
        selectors: &[SpaceSelector],
        below: bool,
    ) -> Result<Vec<Located>> {
        let mut roots = Vec::new();
        for selector in selectors {
            roots.push(self.locate_space(caller, selector).await?);
        }
        // Without the spaces below, the walk only drops repeated roots.
        let tree = match below {
            true => Tree {
                here: self.visible_spaces(caller).await?,
                view: self.view_below(caller).await,
            },
            false => Tree {
                here: Vec::new(),
                view: None,
            },
        };
        Ok(tree.walk(roots))
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

    /// The hub's view of the other machines' spaces `caller` sees, which a
    /// walk down the tree crosses into; none, logged, while it cannot be
    /// asked.
    async fn view_below(&self, caller: Option<&Caller>) -> Option<HubView> {
        match self.hub.hub_view(self, caller).await {
            Ok(view) => view,
            Err(error) => {
                log!("the other machines' spaces below are left out: {error:#}");
                None
            }
        }
    }
}

/// The spaces a walk down the tree finds below a space: this machine's
/// the caller sees, and the other machines' the hub lists.
struct Tree {
    here: Vec<Space>,
    view: Option<HubView>,
}

impl Tree {
    /// `roots` with the spaces below each, each once and after the space
    /// above it, crossing between machines as often as the tree does.
    fn walk(&self, roots: Vec<Located>) -> Vec<Located> {
        let mut queue = VecDeque::from(roots);
        let mut seen = HashSet::new();
        let mut found = Vec::new();
        while let Some(next) = queue.pop_front() {
            if seen.insert(next.id().clone()) {
                queue.extend(self.children(next.id()));
                found.push(next);
            }
        }
        found
    }

    /// The spaces right below the space `id`, on every machine.
    fn children(&self, id: &SpaceId) -> Vec<Located> {
        let here = (self.here.iter())
            .filter(|space| self.parent_of(space) == Some(id))
            .map(|space| Located::Here(Box::new(space.clone())));
        let elsewhere = self.view.iter().flat_map(|view| {
            (view.directory.spaces.iter())
                .filter(|listed| listed.space.parent.as_ref() == Some(id))
                .map(|listed| located_elsewhere(view, listed))
        });
        here.chain(elsewhere).collect()
    }

    /// The space above `space`, here or, as the hub lists this machine's
    /// spaces below another machine's, elsewhere.
    fn parent_of<'a>(&'a self, space: &'a Space) -> Option<&'a SpaceId> {
        if let Some(parent) = &space.parent {
            return Some(&parent.id);
        }
        let below_elsewhere = self.view.iter().flat_map(|view| &view.below_elsewhere);
        (below_elsewhere.into_iter())
            .find(|(below, _)| below == &space.id)
            .map(|(_, parent)| parent)
    }
}

/// The other machine's space `listed`, as `view` lists it, with that
/// machine, named by its ID where the directory lists no such machine.
fn located_elsewhere(view: &HubView, listed: &Listed) -> Located {
    let machine = view.directory.machine(&listed.machine);
    Located::Elsewhere {
        space: Box::new(listed.clone()),
        machine: Machine {
            id: listed.machine.clone(),
            name: view.machine_name(listed).to_owned(),
        },
        online: machine.is_some_and(|machine| machine.connected),
    }
}

#[cfg(test)]
mod tests;
