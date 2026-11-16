//! Reporting a finished command's exit code to the daemon, waiting for a
//! restarting daemon to take the execution back when it went away.
use anyhow::{Context, Result, bail};
use tokio::{
    signal::unix::{SignalKind, signal},
    time::timeout,
};

use super::{
    Mode,
    link::{Link, Received},
};
use crate::protocol::{Control, ExecutionEvent, timing};

/// The command exited, but the daemon retained its unresolved execution.
#[derive(Debug)]
pub struct SetupVerificationFailure {
    pub exit_code: i32,
}

impl std::fmt::Display for SetupVerificationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "setup command exited with status {}, but Shoal could not verify that all processes stopped",
            self.exit_code
        )
    }
}

impl std::error::Error for SetupVerificationFailure {}

/// Report the exit code, resending it to a reattached daemon when the
/// previous one went away before acknowledging it.
pub(super) async fn report_completion(link: &mut Link, code: i32, mode: &Mode) -> Result<bool> {
    loop {
        if !link.is_attached() {
            await_reattachment(link).await?;
        }
        link.send(&ExecutionEvent::Finished { exit_code: code })
            .await?;
        let exchange = async {
            loop {
                match link.recv().await? {
                    Received::Control(Control::Finished { complete }) => {
                        return Ok::<_, anyhow::Error>(Some(complete));
                    }
                    Received::Control(_) => {}
                    Received::Detached | Received::Reattached => return Ok(None),
                }
            }
        };
        let acknowledged = timeout(timing::COMPLETION_ACK_TIMEOUT, exchange)
            .await
            .context("execution completion acknowledgement timed out")??;
        let Some(complete) = acknowledged else {
            continue;
        };
        if !complete {
            if mode.is_setup() {
                return Err(SetupVerificationFailure { exit_code: code }.into());
            }
            eprintln!(
                "warning: execution has surviving or unverified processes; shoal stop stops them, naming any it cannot"
            );
        }
        return Ok(complete);
    }
}

/// Wait for a restarting daemon to take the execution back after its
/// command exited. The terminal is the caller's again, so a signal ends it.
pub(super) async fn await_reattachment(link: &mut Link) -> Result<()> {
    eprintln!("shoal: waiting for the daemon to restart to report the command's exit");
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut quit = signal(SignalKind::quit())?;
    let reattached = async {
        while !matches!(link.recv().await?, Received::Reattached) {}
        Ok::<_, anyhow::Error>(())
    };
    tokio::select! {
        reattached = timeout(timing::REATTACH_EXIT_TIMEOUT, reattached) => {
            reattached.context("the daemon did not restart")?
        }
        _ = terminate.recv() => bail!("interrupted while waiting for the daemon"),
        _ = interrupt.recv() => bail!("interrupted while waiting for the daemon"),
        _ = quit.recv() => bail!("interrupted while waiting for the daemon"),
    }
}
