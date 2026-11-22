//! Which spaces have an agent session to attach to, where the terminal
//! hosts of this machine keep them.
use std::collections::{HashMap, HashSet};

use shoal_ids::space_ids::SpaceId;

use crate::{
    config::Effective,
    daemon::{log, manager::Manager},
    model::Space,
    sessions::Origin,
};

impl Manager {
    /// The IDs of `spaces` with an agent session, counting the places their
    /// connected agents were recorded in, including one Shoal stopped whose
    /// wrapper waits to restore it, which attaching resumes, and those Shoal
    /// created for them, such as a session left at its shell after a failed
    /// launch. A record the daemon cannot read counts as no session.
    pub(crate) async fn session_spaces(&self, spaces: &[Space]) -> HashSet<SpaceId> {
        let agents = (self.agents.running_agents().await).unwrap_or_else(|error| {
            log!("agents unreadable for their sessions: {error:#}");
            Vec::new()
        });
        let owned = (self.agents.owned_places(None).await).unwrap_or_else(|error| {
            log!("places unreadable for their sessions: {error:#}");
            Vec::new()
        });
        let owned = owned.iter().map(|(space, origin)| (space, origin));
        let known = crate::terminal::known_sessions(&agents, owned);
        let settings = self.place_settings(spaces, &known.to_ask).await;
        let mut owners = known.owners;
        owners.extend(crate::terminal::open_session_owners(&known.to_ask, &settings).await);
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
