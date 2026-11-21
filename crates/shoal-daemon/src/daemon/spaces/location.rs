//! The spaces the caller sees across the joined machines of the hub: every
//! one, where those a request names run, and the spaces below them.
use std::collections::{HashSet, VecDeque};

use anyhow::{Context as _, Result};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::{
    hub::{
        Member,
        directory::{HubView, Listed},
    },
    model::{Everywhere, Located, Space, Spaces},
    space_names::SpaceSelector,
};

use crate::daemon::{log, manager::Manager, scope::Caller};

impl Manager {
    /// The spaces `caller` sees on every machine: this machine's as a
    /// listing shows them, unless not `here`, and the other machines' as
    /// the hub lists them, or why the hub could not be asked.
    pub(crate) async fn spaces_everywhere(
        &self,
        caller: Option<&Caller>,
        here: bool,
    ) -> Result<Everywhere> {
        let here = match here {
            true => Spaces(self.listed_spaces(caller).await?),
            false => Spaces::default(),
        };
        Ok(match self.hub.hub_view(self, caller).await {
            Ok(elsewhere) => Everywhere {
                here,
                elsewhere,
                unreachable: None,
            },
            Err(error) => Everywhere {
                here,
                elsewhere: None,
                unreachable: Some(format!("{error:#}")),
            },
        })
    }

    /// Where the space `selector` names runs, among those `caller` sees:
    /// here, with its record, else on the other joined machine the hub
    /// lists it on; refused as absent when neither has it, naming a removed
    /// space it names by ID.
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
        Err(self.missing_space(selector).await)
    }

    /// The spaces `selectors` name, located as [`Self::locate_space`]
    /// locates one, or with `machines` every space of those joined
    /// machines, with the spaces below each on every machine when `below`,
    /// each once and after the space above it; a hub that cannot be asked
    /// leaves out the other machines' spaces below.
    pub(crate) async fn select_spaces(
        &self,
        caller: Option<&Caller>,
        selectors: &[SpaceSelector],
        machines: &[String],
        below: bool,
    ) -> Result<Vec<Located>> {
        let roots = match machines.is_empty() {
            true => self.located(caller, selectors).await?,
            false => self.spaces_on(caller, machines).await?,
        };
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

    /// The spaces `selectors` name, each located.
    async fn located(
        &self,
        caller: Option<&Caller>,
        selectors: &[SpaceSelector],
    ) -> Result<Vec<Located>> {
        let mut located = Vec::new();
        for selector in selectors {
            located.push(self.locate_space(caller, selector).await?);
        }
        Ok(located)
    }

    /// Every space `caller` sees of the joined machines `machines` names,
    /// by name or ID: this machine's but the caller's own, and another's as
    /// the hub lists them.
    async fn spaces_on(
        &self,
        caller: Option<&Caller>,
        machines: &[String],
    ) -> Result<Vec<Located>> {
        let view = (self.hub.hub_view(self, caller).await?).context(
            "this machine joined no hub, so it runs every space it has; leave out --machine",
        )?;
        let mut found = Vec::new();
        for named in machines {
            let machine = (view.directory.named_machine(named)).map_err(anyhow::Error::msg)?;
            match machine.id == view.machine {
                true => found.extend(self.reached_here(caller).await?),
                false => found.extend(listed_on(&view, &machine.id)),
            }
        }
        Ok(found)
    }

    /// The spaces of this machine `caller`'s scope reaches but its own, as
    /// it may restart or stop them.
    async fn reached_here(&self, caller: Option<&Caller>) -> Result<Vec<Located>> {
        let visible = self.visible_spaces(caller).await?;
        let reached = self.spaces_below(caller, visible).await?;
        Ok((reached.into_iter())
            .map(|space| Located::Here(Box::new(space)))
            .collect())
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

/// The spaces `view` lists on the machine `machine`.
fn listed_on<'a>(view: &'a HubView, machine: &'a MemberId) -> impl Iterator<Item = Located> + 'a {
    (view.directory.spaces.iter())
        .filter(move |listed| &listed.machine == machine)
        .map(|listed| located_elsewhere(view, listed))
}

/// The other machine's space `listed`, as `view` lists it, with that
/// machine, one never seen where the hub's members leave it out.
fn located_elsewhere(view: &HubView, listed: &Listed) -> Located {
    let machine = (view.directory.machine(&listed.machine).cloned())
        .unwrap_or_else(|| Member::unlisted(listed.machine.clone()));
    Located::Elsewhere {
        space: Box::new(listed.clone()),
        machine: Box::new(machine),
    }
}

#[cfg(test)]
mod tests;
