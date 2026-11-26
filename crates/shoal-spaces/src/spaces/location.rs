//! The spaces the caller sees across the joined machines of the hub, and
//! where those a request names run.
use crate::Spaces;
use crate::daemon::Daemon;
use std::collections::{HashSet, VecDeque};

use anyhow::{Context as _, Result};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::{
    hub::{
        Member,
        directory::{HubView, Listed},
    },
    model::{Everywhere, Located, Space, Spaces as Listing},
    protocol::Operation,
    space_names::SpaceSelector,
};

use crate::{log, scope::Caller};

impl Spaces {
    /// This machine's as a listing shows them, unless not `here`, and the
    /// other machines' as the hub lists them, or why the hub could not be
    /// asked. With `acting`, the request a picker offers them for, the
    /// daemon of the machine each runs on judges it. Then none of the other
    /// machines' are listed for a request that stays on this one.
    pub async fn spaces_everywhere(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        here: bool,
        acting: Option<&Operation>,
    ) -> Result<Everywhere> {
        let listed = match here || acting.is_some() {
            true => self.listed_spaces(daemon, caller).await?,
            false => Vec::new(),
        };
        let offers = match acting {
            Some(acting) => self.judged_here(daemon, caller, acting, &listed).await,
            None => Vec::new(),
        };
        let here = match here {
            true => Listing(listed),
            false => Listing::default(),
        };
        let mut everywhere = Everywhere {
            here,
            offers: (offers.into_iter())
                .map(|judged| (judged.space, judged.offer))
                .collect(),
            ..Everywhere::default()
        };
        if acting.is_some_and(|acting| !acting.reaches_other_machines()) {
            return Ok(everywhere);
        }
        match daemon.hub_view(caller).await {
            Ok(elsewhere) => everywhere.elsewhere = elsewhere,
            Err(error) => everywhere.unreachable = Some(format!("{error:#}")),
        }
        if let (Some(acting), Some(view)) = (acting, &everywhere.elsewhere) {
            let judged = self.offers_elsewhere(daemon, caller, acting, view).await;
            everywhere.offers.extend(judged);
        }
        Ok(everywhere)
    }

    /// Here, with its record, else on the other joined machine the hub
    /// lists it on. Refused as absent when neither has it, naming a removed
    /// space it names by ID.
    pub async fn locate_space(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
    ) -> Result<Located> {
        if let Some(space) = self.seen_here(daemon, caller, selector).await? {
            return Ok(Located::Here(Box::new(space)));
        }
        let view = (daemon.hub_view(caller).await)
            .with_context(|| format!("no space {selector} here, and the hub cannot be asked"))?;
        if let Some(view) = &view
            && let Some(listed) = view.selected(selector).map_err(anyhow::Error::msg)?
        {
            return Ok(located_elsewhere(view, listed));
        }
        Err(self.missing_space(selector).await)
    }

    /// With `machines`, every space of those joined machines. With `below`,
    /// the spaces below each on every machine, each once and after the space
    /// above it. A hub that cannot be asked leaves out the other machines'
    /// spaces below.
    pub async fn select_spaces(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        selectors: &[SpaceSelector],
        machines: &[String],
        below: bool,
    ) -> Result<Vec<Located>> {
        let roots = match machines.is_empty() {
            true => self.located(daemon, caller, selectors).await?,
            false => self.spaces_on(daemon, caller, machines).await?,
        };
        // Without the spaces below, the walk only drops repeated roots.
        let tree = match below {
            true => Tree {
                here: daemon.coordination().visible_spaces(daemon, caller).await?,
                view: self.view_below(daemon, caller).await,
            },
            false => Tree {
                here: Vec::new(),
                view: None,
            },
        };
        Ok(tree.walk(roots))
    }

    async fn located(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        selectors: &[SpaceSelector],
    ) -> Result<Vec<Located>> {
        let mut located = Vec::new();
        for selector in selectors {
            located.push(self.locate_space(daemon, caller, selector).await?);
        }
        Ok(located)
    }

    /// `machines` names them by name or ID. This machine's leave out the
    /// caller's own; another's are as the hub lists them.
    async fn spaces_on(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        machines: &[String],
    ) -> Result<Vec<Located>> {
        let view = (daemon.hub_view(caller).await?).context(
            "this machine joined no hub, so it runs every space it has; leave out --machine",
        )?;
        let mut found = Vec::new();
        for named in machines {
            let machine = (view.directory.named_machine(named)).map_err(anyhow::Error::msg)?;
            match machine.id == view.machine {
                true => found.extend(self.reached_here(daemon, caller).await?),
                false => found.extend(listed_on(&view, &machine.id)),
            }
        }
        Ok(found)
    }

    /// Leaves out the caller's own, as it may restart or stop these.
    pub async fn reached_here(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
    ) -> Result<Vec<Located>> {
        let visible = daemon.coordination().visible_spaces(daemon, caller).await?;
        let reached = daemon
            .coordination()
            .spaces_below(daemon, caller, visible)
            .await?;
        Ok((reached.into_iter())
            .map(|space| Located::Here(Box::new(space)))
            .collect())
    }

    async fn seen_here(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
    ) -> Result<Option<Space>> {
        let spaces = daemon.coordination().visible_spaces(daemon, caller).await?;
        Ok((spaces.into_iter()).find(|space| selector.selects(&space.id, space.number)))
    }

    /// A walk down the tree crosses into it. None, logged, while the hub
    /// cannot be asked.
    async fn view_below(&self, daemon: &dyn Daemon, caller: Option<&Caller>) -> Option<HubView> {
        match daemon.hub_view(caller).await {
            Ok(view) => view,
            Err(error) => {
                log!("the other machines' spaces below are left out: {error:#}");
                None
            }
        }
    }
}

/// This machine's spaces the caller sees, and the other machines' the hub
/// lists.
pub struct Tree {
    pub here: Vec<Space>,
    pub view: Option<HubView>,
}

impl Tree {
    /// Each once and after the space above it, crossing between machines as
    /// often as the tree does.
    pub fn walk(&self, roots: Vec<Located>) -> Vec<Located> {
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

    /// On every machine.
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

    /// Here, or elsewhere, as the hub lists this machine's spaces below
    /// another machine's.
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

fn listed_on<'a>(view: &'a HubView, machine: &'a MemberId) -> impl Iterator<Item = Located> + 'a {
    (view.directory.spaces.iter())
        .filter(move |listed| &listed.machine == machine)
        .map(|listed| located_elsewhere(view, listed))
}

/// A machine the hub's members leave out is one never seen.
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
