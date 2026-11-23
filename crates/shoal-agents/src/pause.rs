//! The user pausing this machine until they unpause it: the agent queue
//! admits nothing new, no agent, restore, CI job or review, which the
//! store keeps through daemon restarts and the hub's placement reads from
//! the machine's capacity, and every agent stops as before a shutdown, the
//! spaces with something to do going to other machines of the hub.
//! Unpausing admits work again, which restores the agents that stayed, and
//! retires this machine's copies of the spaces that went.
use std::time::Duration;

use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_protocol::time::UnixTime;

use crate::{
    Agents,
    daemon::Daemon,
    refusal::{Reason, Refusal},
    shutdown_stop::{HandoverWindow, Shutdown},
};

/// The longest a pause's hand-over waits for the hub to confirm its
/// packages.
const PAUSE_HANDOVER: Duration = Duration::from_secs(10 * 60);

impl Agents {
    /// Hold admission while the store records a pause, as the daemon
    /// starts.
    pub async fn keep_pause(&self) -> Result<()> {
        let since = self.store.read(paused_at).await?;
        self.agent_queue.pause(since);
        Ok(())
    }

    /// Since when the user paused this machine, none while it is not.
    pub fn paused_since(&self) -> Option<UnixTime> {
        self.agent_queue.paused_since()
    }

    /// Pause this machine, so it admits nothing new, keeping since when an
    /// earlier pause stands: since when it is paused, and the stop of its
    /// work to run, which refuses an unpause from now on until it ends;
    /// none while an earlier pause's stop runs, which this one leaves to it.
    pub async fn pause(&self) -> Result<(UnixTime, Option<PauseStop>)> {
        let stop = self.pause_stop.clone().try_lock_owned();
        let stop = stop.ok().map(|held| PauseStop { _held: held });
        let since = self
            .store
            .write(|tx| record_pause(tx, UnixTime::now()))
            .await?;
        self.agent_queue.pause(Some(since));
        Ok((since, stop))
    }

    /// Run the `stop` of the paused machine's work: stop every agent here,
    /// handing the spaces with something to do over, and end the CI jobs
    /// to rerun.
    pub async fn stop_for_pause(&self, daemon: &dyn Daemon, stop: PauseStop) {
        let _stopping = stop;
        let window = HandoverWindow::Own(PAUSE_HANDOVER);
        (self.stop_for_shutdown_of(daemon, Shutdown::Pause, window)).await;
    }

    /// Unpause this machine: admit work again, which restores the agents
    /// the pause stopped here, and retire the copies of the spaces it
    /// handed over. Returns since when it was paused, none when it was not;
    /// refused while the pause still stops work and hands spaces over,
    /// since a space it sends may then run here as well.
    pub async fn unpause(&self, daemon: &dyn Daemon) -> Result<Option<UnixTime>> {
        let Ok(_stopped) = self.pause_stop.try_lock() else {
            return Err(Refusal::new(
                Reason::NotReady,
                "this machine's pause still stops its work and hands its spaces over; shoal unpause works once the handed_over alert says where they went",
            )
            .into());
        };
        let was = self.store.write(clear_pause).await?;
        self.agent_queue.pause(None);
        if was.is_some() {
            self.stay_unless_shut_down(daemon).await;
        }
        Ok(was)
    }
}

/// The stop of a paused machine's work, held from the pause until it
/// ends, through which an unpause is refused.
pub struct PauseStop {
    _held: tokio::sync::OwnedMutexGuard<()>,
}

/// Since when the store records this machine paused.
fn paused_at(tx: &Transaction<'_>) -> Result<Option<UnixTime>> {
    let since = tx.query_row("SELECT paused_at FROM machine_pause", [], |row| row.get(0));
    Ok(since.optional()?)
}

/// Record this machine paused from `now`, unless it already is; since when
/// it is.
fn record_pause(tx: &Transaction<'_>, now: UnixTime) -> Result<UnixTime> {
    tx.execute(
        "INSERT OR IGNORE INTO machine_pause(id,paused_at) VALUES (1,?1)",
        params![now],
    )?;
    Ok(paused_at(tx)?.unwrap_or(now))
}

/// Forget this machine's pause; since when it was paused.
fn clear_pause(tx: &Transaction<'_>) -> Result<Option<UnixTime>> {
    let was = paused_at(tx)?;
    tx.execute("DELETE FROM machine_pause", [])?;
    Ok(was)
}

#[cfg(test)]
mod tests;
