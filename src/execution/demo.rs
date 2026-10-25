//! A demo's restarts: its wrapper runs the command again whenever it exits,
//! waiting longer after each exit that follows soon after a start, so a
//! command that fails at once keeps its error on screen without spinning.
use anyhow::{Result, bail};
use tokio::{
    signal::unix::{SignalKind, signal},
    time::{Duration, sleep},
};

use super::link::{Link, Received};
use crate::protocol::Control;

/// A run at least this long counts as having started properly.
const SETTLED: Duration = Duration::from_secs(10);
const FIRST_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(30);

/// The delay before the next restart.
#[derive(Debug, Default)]
pub(super) struct Restarts {
    delay: Option<Duration>,
}

impl Restarts {
    /// The wait before restarting a command that ran for `ran`: none after a
    /// settled run, otherwise one that doubles with each quick exit.
    pub(super) fn after(&mut self, ran: Duration) -> Duration {
        let delay = if ran >= SETTLED {
            Duration::ZERO
        } else {
            self.delay
                .map_or(FIRST_DELAY, |delay| (delay * 2).min(MAX_DELAY))
        };
        self.delay = (!delay.is_zero()).then_some(delay);
        delay
    }
}

/// Wait `delay` before the restart, returning why the demo stopped instead
/// when the daemon or a signal ends it first.
pub(super) async fn await_restart(link: &mut Link, delay: Duration) -> Result<Option<String>> {
    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let wait = sleep(delay);
    tokio::pin!(wait);
    loop {
        tokio::select! {
            () = &mut wait => return Ok(None),
            _ = hangup.recv() => return Ok(Some("its terminal closed".into())),
            _ = terminate.recv() => return Ok(Some("its session was terminated".into())),
            _ = interrupt.recv() => return Ok(Some("interrupted".into())),
            received = link.recv() => match received? {
                Received::Detached | Received::Reattached => {}
                Received::Control(
                    Control::Stop { reason }
                    | Control::Pause { reason, .. },
                ) => return Ok(Some(reason.unwrap_or_else(|| "stopped".into()))),
                Received::Control(Control::OverloadStop { reason, .. }) => return Ok(Some(reason)),
                Received::Control(_) => bail!("unexpected execution control"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_exits_back_off_and_a_settled_run_restarts_at_once() {
        let mut restarts = Restarts::default();
        let quick = Duration::from_secs(1);
        let delays: Vec<_> = (0..7).map(|_| restarts.after(quick).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(restarts.after(SETTLED), Duration::ZERO);
        assert_eq!(restarts.after(quick), FIRST_DELAY);
    }
}
