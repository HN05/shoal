//! Drain the running daemon before starting an OS-enforced service stop.
use super::{
    LABEL, Platform, UNIT,
    control::{SERVICE_STOP_TIMEOUT, command, domain, stop_command},
};
use crate::{paths::Paths, protocol::timing, tools::Tool};
use anyhow::{Context, Result, ensure};
use std::{fs::File, io};
use tokio::time::{Instant, sleep};

pub(super) async fn stop(paths: &Paths, platform: Platform) -> Result<()> {
    match platform {
        Platform::Mac => {
            let target = format!("{}/{LABEL}", domain());
            if command(Tool::Launchctl, &["print", &target], false).await? {
                let signaled =
                    command(Tool::Launchctl, &["kill", "SIGTERM", &target], false).await?;
                let _drained = await_exit(paths, signaled).await?;
                stop_command(Tool::Launchctl, &["bootout", &target]).await?;
            }
        }
        Platform::Linux => {
            let signaled = command(
                Tool::Systemctl,
                &["--user", "kill", "--kill-whom=main", "--signal=TERM", UNIT],
                false,
            )
            .await?;
            let _drained = await_exit(paths, signaled).await?;
            stop_command(Tool::Systemctl, &["--user", "stop", UNIT]).await?;
        }
    }
    Ok(())
}

/// The daemon holds this lock through its entire drain, across exec updates
/// and regardless of wire protocol. Keep the acquired lock through service
/// removal so a replacement cannot acquire the state during that gap.
async fn await_exit(paths: &Paths, signaled: bool) -> Result<Option<File>> {
    let file = match File::open(paths.daemon_lock()) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("open daemon ownership lock"),
    };
    let deadline = Instant::now() + SERVICE_STOP_TIMEOUT;
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ensure!(
                    signaled,
                    "service manager could not signal the running daemon; service left loaded; retry shoal daemon stop"
                );
                ensure!(
                    Instant::now() < deadline,
                    "daemon did not finish draining; service left loaded; retry shoal daemon stop"
                );
                sleep(timing::DAEMON_POLL_INTERVAL).await;
            }
            Err(error) => return Err(error).context("check daemon ownership lock"),
        }
    }
}

#[cfg(test)]
mod tests;
