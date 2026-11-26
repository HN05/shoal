//! The daemon log's record of a wake hook that ends without starting its
//! agent's turn, so that an idle agent can be traced to how its hook ended.
use anyhow::Result;

use shoal_daemon_base::log;
use shoal_protocol::{links::Updates, protocol::WakeEnd, space_names::SpaceSelector};

use crate::{Links, daemon::Daemon};

impl Links {
    /// Log that the wake hook of the agent of the workspace or running chat
    /// `selector` names ended for `ended` without starting the agent's turn.
    pub async fn report_wake_end(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        ended: &WakeEnd,
    ) -> Result<()> {
        let space = daemon.space(selector).await?;
        let name = &space.name;
        match ended {
            WakeEnd::AgentExited => log!("wake hook of {name} ended: its agent exited"),
            WakeEnd::AgentUnknown => {
                log!("wake hook of {name} ended: its agent's process could not be inspected");
            }
            WakeEnd::Stopped => {
                log!("wake hook of {name} ended: a stop of its agent came before the turn");
            }
            WakeEnd::Signalled { signal } => log!("wake hook of {name} ended by {signal}"),
        }
        Ok(())
    }
}

/// How a wake hook's wait ended without updates.
#[derive(Debug, PartialEq, Eq)]
enum Ending {
    /// A newer delivery or watch took the updates, and the hook ends.
    Superseded,
    /// The wait failed, and the hook tries again.
    Failed(String),
    /// No answer: the hook hung up, or the daemon stopped.
    Unanswered,
}

/// Logs how the wait of the wake hook of the space `name` ended, unless it
/// answered with updates.
pub struct WakeWaitLog {
    name: String,
    answered: bool,
}

impl WakeWaitLog {
    pub fn start(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            answered: false,
        }
    }

    /// Log the wait's `answer`, unless it holds updates.
    pub fn answered(mut self, answer: &Result<Updates>) {
        self.answered = true;
        if let Some(ending) = ending(answer) {
            self.log(&ending);
        }
    }

    fn log(&self, ending: &Ending) {
        let name = &self.name;
        match ending {
            Ending::Superseded => {
                log!("wake wait of {name} ended: a newer delivery or watch took its updates");
            }
            Ending::Failed(error) => log!("wake wait of {name} failed: {error}"),
            Ending::Unanswered => {
                log!(
                    "wake wait of {name} ended unanswered: its hook hung up or the daemon stopped"
                );
            }
        }
    }
}

impl Drop for WakeWaitLog {
    fn drop(&mut self) {
        if !self.answered {
            self.log(&Ending::Unanswered);
        }
    }
}

/// How a wait that gave `answer` ended, unless with updates.
fn ending(answer: &Result<Updates>) -> Option<Ending> {
    match answer {
        Err(error) => Some(Ending::Failed(format!("{error:#}"))),
        Ok(updates) => updates.superseded.then_some(Ending::Superseded),
    }
}

#[cfg(test)]
mod tests;
