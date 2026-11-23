//! The user walking away from this machine and coming back, which the
//! daemon reads from the platform while `keep_awake` acts on it: the agent
//! queue admits nothing new while they are away, and keep-awake holds the
//! machine only as the level says then. Under `handover`, the spaces with
//! something to do go to other machines of the hub once the user walks
//! away, or before the machine sleeps, and this machine retires its copies
//! once they come back.
use std::{collections::HashSet, time::Duration};

use anyhow::Result;
use shoal_core::config::machine::KeepAwake;
use shoal_ids::space_ids::SpaceId;
use tokio::{sync::watch, time::Instant};

use crate::{
    Agents,
    daemon::Daemon,
    queue::store::queued_rows,
    shutdown_stop::{HandoverWindow, Shutdown},
    state::{AgentState, QueueKind},
    turn_state,
};

/// The longest a hand-over once the user walked away holds the machine
/// awake for the hub to confirm its packages.
const AWAY_HANDOVER: Duration = Duration::from_secs(10 * 60);

/// Where the user is, as `keep_awake` acts on it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Presence {
    /// They walked away under a `keep_awake` that admits nothing new then.
    pub away: bool,
    /// The spaces with something to do are going to other machines.
    pub handing_over: bool,
}

/// What [`Agents`] keeps of the user's presence.
pub struct Presences {
    presence: watch::Sender<Presence>,
    /// Held through a hand-over; true once one finished, until the user
    /// is back.
    handed_over: tokio::sync::Mutex<bool>,
}

impl Default for Presences {
    fn default() -> Self {
        Self {
            presence: watch::Sender::new(Presence::default()),
            handed_over: Default::default(),
        }
    }
}

impl Presences {
    fn set(&self, change: impl FnOnce(&mut Presence)) {
        self.presence.send_if_modified(|presence| {
            let before = *presence;
            change(presence);
            *presence != before
        });
    }
}

impl Agents {
    /// The user's presence, which keep-awake follows.
    pub fn presence(&self) -> watch::Receiver<Presence> {
        self.presences.presence.subscribe()
    }

    /// The user is `away` from this machine, or back, under a `keep_awake`
    /// that admits nothing new while they are away; the daemon says so at
    /// each reading. Under `handover`, the spaces with something to do go
    /// to other machines once; once the user is back, this machine admits
    /// work again and retires the copies of those that went.
    pub async fn user_away(&self, daemon: &dyn Daemon, away: bool) {
        self.agent_queue.away(away);
        self.presences.set(|presence| presence.away = away);
        match away {
            true if self.config.current().machine.keep_awake == KeepAwake::Handover => {
                let window = HandoverWindow::Own(AWAY_HANDOVER);
                self.hand_over_once(daemon, Shutdown::Away, window).await;
            }
            true => {}
            false => self.come_back(daemon).await,
        }
    }

    /// Hand the spaces with something to do over before the machine sleeps
    /// by `deadline`, under `keep_awake = "handover"`, as when the user
    /// walks away.
    pub async fn hand_over_before_sleep(&self, daemon: &dyn Daemon, deadline: Instant) {
        if self.config.current().machine.keep_awake != KeepAwake::Handover {
            return;
        }
        let window = HandoverWindow::Until(deadline);
        self.hand_over_once(daemon, Shutdown::Sleep, window).await;
    }

    /// Stop the spaces with something to do and hand them over within
    /// `window`, holding the machine awake meanwhile, unless they went
    /// since the user was last here or this machine hands nothing over.
    async fn hand_over_once(&self, daemon: &dyn Daemon, why: Shutdown, window: HandoverWindow) {
        let mut handed_over = self.presences.handed_over.lock().await;
        if *handed_over || !daemon.hands_over().await {
            return;
        }
        self.presences.set(|presence| presence.handing_over = true);
        self.stop_for_shutdown_of(daemon, why, window).await;
        self.presences.set(|presence| presence.handing_over = false);
        *handed_over = true;
    }

    /// Admit work again once the user is back after a hand-over, retiring
    /// the copies of the spaces that went.
    async fn come_back(&self, daemon: &dyn Daemon) {
        let mut handed_over = self.presences.handed_over.lock().await;
        if std::mem::take(&mut *handed_over) {
            self.stay(daemon).await;
        }
    }

    /// The spaces with something to do: an agent working on a turn, or
    /// waiting in the queue to start or be restored.
    pub(crate) async fn spaces_with_work(&self) -> Result<HashSet<SpaceId>> {
        self.store
            .read(|tx| {
                let working = (turn_state::all(tx)?.into_iter())
                    .filter(|(_, state)| *state == AgentState::Working)
                    .map(|(id, _)| id);
                let queued = (queued_rows(tx)?.into_iter())
                    .filter(|row| row.kind != QueueKind::Job)
                    .filter_map(|row| row.workspace);
                Ok(working.chain(queued).collect())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
