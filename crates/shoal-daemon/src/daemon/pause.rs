//! Pausing and unpausing this machine, or another joined machine where the
//! hub carries the request: a pause admits no new work and then stops what
//! runs in the background, the agents as before a shutdown, handing the
//! spaces with something to do over, and the CI jobs, reviews and demos;
//! an unpause admits work again.
use std::sync::Arc;

use anyhow::{Context as _, Result};
use shoal_protocol::{
    hub::Member,
    pause::{Paused, Unpaused},
    protocol::{Body, MachineMethod, Operation},
};

use super::{log, manager::Manager};

/// What the demos a pause stops are told.
const PAUSE_REASON: &str = "stopped as the user paused this machine";

impl Manager {
    /// Pause the joined machine `machine` names, or this one: admit no new
    /// work, then [stop what runs](Self::stop_paused_work) in the
    /// background, which a hand-over may take minutes to finish.
    pub(super) async fn pause(self: &Arc<Self>, machine: Option<&str>) -> Result<Paused> {
        if let Some(machine) = self.hub.other_machine(&**self, machine).await? {
            let method = MachineMethod::PauseMachine { machine: None };
            let body = (self.ask_machine(&machine, method).await)
                .with_context(|| format!("pause machine {}", machine.name))?;
            return Paused::try_from(body);
        }
        let since = self.agents.pause().await?;
        self.hub.capacity_changed();
        tokio::spawn(self.clone().stop_paused_work());
        Ok(Paused { since })
    }

    /// Stop what runs on this paused machine: the agents, handing the
    /// spaces with something to do over, the CI jobs, to rerun elsewhere,
    /// the running reviews, to run again once unpaused, and the demos.
    async fn stop_paused_work(self: Arc<Self>) {
        let ((), (), demos) = tokio::join!(
            self.agents.stop_for_pause(&*self),
            self.reviews.stop_to_run_later(),
            self.demos.stop_all(&*self, PAUSE_REASON.to_owned()),
        );
        if let Err(error) = demos {
            log!("pause: could not stop every demo: {error:#}");
        }
    }

    /// Unpause the joined machine `machine` names, or this one, which then
    /// admits work again.
    pub(super) async fn unpause(&self, machine: Option<&str>) -> Result<Unpaused> {
        if let Some(machine) = self.hub.other_machine(self, machine).await? {
            let method = MachineMethod::UnpauseMachine { machine: None };
            let body = (self.ask_machine(&machine, method).await)
                .with_context(|| format!("unpause machine {}", machine.name))?;
            return Unpaused::try_from(body);
        }
        let paused_since = self.agents.unpause(self).await?;
        self.hub.capacity_changed();
        Ok(Unpaused { paused_since })
    }

    /// What `machine` answers `method` with, as the user's, through the
    /// hub; it fails at once, naming the machine, while that machine is
    /// offline.
    async fn ask_machine(&self, machine: &Member, method: MachineMethod) -> Result<Body> {
        let operation = Operation::Machine(method);
        (self.hub.ask_machine(self, machine, operation, None)).await
    }
}
