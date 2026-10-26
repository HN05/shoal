//! A demo's restarts: its wrapper runs the command again whenever it exits,
//! waiting longer after each exit that follows soon after a start, so a
//! command that fails at once keeps its error on screen without spinning.
use anyhow::{Context, Result, bail};
use tokio::{
    signal::unix::{SignalKind, signal},
    time::{Duration, sleep, timeout},
};

use super::link::{Link, Received};
use crate::protocol::{Control, ExecutionEvent};

/// A run at least this long counts as having started properly.
const SETTLED: Duration = Duration::from_secs(10);
const FIRST_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(30);
/// A hang guard for the daemon's answer, which waits for proof that the
/// exited run's processes stopped.
const RELEASE_LIMIT: Duration = Duration::from_secs(60);

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

/// Prepare the next run after the child exited: wait `delay`, then have the
/// daemon release the child and check the worktree, so the check holds when
/// the next run starts. Returns why the demo stopped instead when a stop
/// comes first or the daemon refuses the next run.
pub(super) async fn next_run(link: &mut Link, delay: Duration) -> Result<Option<String>> {
    if let Some(reason) = await_restart(link, delay).await? {
        return Ok(Some(reason));
    }
    release_child(link).await
}

/// Report the exited child until the daemon answers, telling a restarted
/// daemon again, since it may have gone before the report arrived. From the
/// report until the next run registers, a reattachment says the daemon may
/// have forgotten the child.
async fn release_child(link: &mut Link) -> Result<Option<String>> {
    link.between_runs();
    loop {
        if !link.is_attached() {
            super::completion::await_reattachment(link).await?;
        }
        link.send(&ExecutionEvent::ChildExited).await?;
        let answer = async {
            loop {
                match link.recv().await? {
                    Received::Control(Control::ChildReleased) => return Ok(Some(None)),
                    Received::Control(Control::Stop { reason } | Control::Pause { reason, .. }) => {
                        return Ok(Some(Some(reason.unwrap_or_else(|| "stopped".into()))));
                    }
                    Received::Control(Control::OverloadStop { reason, .. }) => {
                        return Ok(Some(Some(reason)));
                    }
                    Received::Control(_) => {}
                    Received::Detached | Received::Reattached => {
                        return Ok::<_, anyhow::Error>(None);
                    }
                }
            }
        };
        let answered = timeout(RELEASE_LIMIT, answer)
            .await
            .context("the daemon did not release the demo's exited run")??;
        if let Some(outcome) = answered {
            return Ok(outcome);
        }
    }
}

/// Wait `delay` before the restart, returning why the demo stopped instead
/// when the daemon or a signal ends it first.
async fn await_restart(link: &mut Link, delay: Duration) -> Result<Option<String>> {
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
mod tests;
