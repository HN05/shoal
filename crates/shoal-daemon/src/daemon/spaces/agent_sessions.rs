//! Which spaces have an agent session to attach to, where the terminal
//! hosts of this machine keep them.
use std::collections::{HashMap, HashSet};

use shoal_ids::space_ids::SpaceId;

use crate::{
    config::Effective,
    daemon::{log, manager::Manager},
    model::{RunningAgent, Space},
    sessions::Origin,
};

impl Manager {
    /// The IDs of `spaces` with an agent session, counting the places their
    /// connected agents were recorded in, including one Shoal stopped whose
    /// wrapper waits to restore it, which attaching resumes, and those Shoal
    /// created for them, such as a session left at its shell after a failed
    /// launch. A record the daemon cannot read counts as no session.
    pub(crate) async fn session_spaces(&self, spaces: &[Space]) -> HashSet<SpaceId> {
        let agents = (self.running_agents().await).unwrap_or_else(|error| {
            log!("agents unreadable for their sessions: {error:#}");
            Vec::new()
        });
        let owned = (self.owned_places(None).await).unwrap_or_else(|error| {
            log!("places unreadable for their sessions: {error:#}");
            Vec::new()
        });
        let places = agent_places(&agents);
        let mut owners = crate::terminal::session_owners(&places.connected);
        // Each host lists its sessions once for the owners still in question.
        let recorded: Vec<_> = (places.unknown.into_iter())
            .chain(owned.iter().map(|(space, origin)| (space, origin)))
            .filter(|(owner, _)| !owners.contains(*owner))
            .collect();
        let settings = self.place_settings(spaces, &recorded).await;
        owners.extend(crate::terminal::open_session_owners(&recorded, &settings).await);
        owners
    }

    /// The settings of each of `spaces` that owns one of `places`, which
    /// say which hosts to ask about them; one whose settings cannot be read
    /// is left out.
    async fn place_settings(
        &self,
        spaces: &[Space],
        places: &[(&SpaceId, &Origin)],
    ) -> HashMap<SpaceId, Effective> {
        let mut settings = HashMap::new();
        for space in spaces {
            let owns = places.iter().any(|(owner, _)| **owner == space.id);
            if !owns || settings.contains_key(&space.id) {
                continue;
            }
            if let Ok(resolved) = self.repositories.space_settings(self, space).await {
                settings.insert(space.id.clone(), resolved);
            }
        }
        settings
    }
}

/// Where agents were recorded running, as (space ID, place) pairs.
#[derive(Debug, Default, PartialEq, Eq)]
struct AgentPlaces<'a> {
    /// Connected agents, which run in their places or, stopped by Shoal,
    /// wait there to be restored.
    connected: Vec<(&'a SpaceId, &'a Origin)>,
    /// Executions whose wrapper is gone, whose record outlives the session.
    unknown: Vec<(&'a SpaceId, &'a Origin)>,
}

/// Sort the places of `agents` by what runs in them; an agent without a
/// recorded place has none.
fn agent_places(agents: &[RunningAgent]) -> AgentPlaces<'_> {
    let mut places = AgentPlaces::default();
    for agent in agents {
        let Some(place) = agent.origin.as_ref() else {
            continue;
        };
        let kind = match &agent.agent {
            None => &mut places.unknown,
            Some(_) => &mut places.connected,
        };
        kind.push((&agent.space_id, place));
    }
    places
}

#[cfg(test)]
mod tests;
