//! The user walking away from this machine and coming back, which the
//! daemon reads from the platform while `keep_awake` acts on it: the agent
//! queue admits nothing new while they are away, and keep-awake holds the
//! machine only as the level says then.
use tokio::sync::watch;

use crate::Agents;

/// Where the user is, as `keep_awake` acts on it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Presence {
    /// They walked away under a `keep_awake` that admits nothing new then.
    pub away: bool,
}

/// What [`Agents`] keeps of the user's presence.
pub struct Presences(watch::Sender<Presence>);

impl Default for Presences {
    fn default() -> Self {
        Self(watch::Sender::new(Presence::default()))
    }
}

impl Agents {
    /// The user's presence, which keep-awake follows.
    pub fn presence(&self) -> watch::Receiver<Presence> {
        self.presences.0.subscribe()
    }

    /// The user is `away` from this machine, or back, under a `keep_awake`
    /// that admits nothing new while they are away; the daemon says so at
    /// each reading, so only a change acts.
    pub fn user_away(&self, away: bool) {
        self.agent_queue.away(away);
        self.presences.0.send_if_modified(|presence| {
            let changed = presence.away != away;
            presence.away = away;
            changed
        });
    }
}

#[cfg(test)]
mod tests;
