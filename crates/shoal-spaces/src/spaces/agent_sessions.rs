//! Which spaces have an agent session to attach to, where this machine's
//! terminal hosts keep them.
use crate::Spaces;
use crate::daemon::Daemon;
use std::collections::{HashMap, HashSet};

use shoal_ids::space_ids::SpaceId;

use crate::{config::Effective, log, model::Space, sessions::Origin};

impl Spaces {
    /// Counts the places their connected agents were recorded in, including
    /// one Shoal stopped whose wrapper waits to restore it, which attaching
    /// resumes, and those Shoal created for them, such as a session left at
    /// its shell after a failed launch. A record the daemon cannot read
    /// counts as no session.
    pub async fn session_spaces(&self, daemon: &dyn Daemon, spaces: &[Space]) -> HashSet<SpaceId> {
        let agents = (daemon.running_agents().await).unwrap_or_else(|error| {
            log!("agents unreadable for their sessions: {error:#}");
            Vec::new()
        });
        let owned = (daemon.owned_places(None).await).unwrap_or_else(|error| {
            log!("places unreadable for their sessions: {error:#}");
            Vec::new()
        });
        let owned = owned.iter().map(|(space, origin)| (space, origin));
        let known = crate::terminal::known_sessions(&agents, owned);
        let settings = self.place_settings(daemon, spaces, &known.to_ask).await;
        let mut owners = known.owners;
        owners.extend(crate::terminal::open_session_owners(&known.to_ask, &settings).await);
        owners
    }

    /// The settings say which hosts to ask. A space whose settings cannot be
    /// read is left out.
    async fn place_settings(
        &self,
        daemon: &dyn Daemon,
        spaces: &[Space],
        places: &[(&SpaceId, &Origin)],
    ) -> HashMap<SpaceId, Effective> {
        let mut settings = HashMap::new();
        for space in spaces {
            let owns = places.iter().any(|(owner, _)| **owner == space.id);
            if !owns || settings.contains_key(&space.id) {
                continue;
            }
            if let Ok(resolved) = daemon.space_settings(space).await {
                settings.insert(space.id.clone(), resolved);
            }
        }
        settings
    }
}
