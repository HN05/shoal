//! macOS's view of the user's presence, as `ioreg` and `pmset` print it:
//! IOKit's `AppleClamshellState` and `AppleClamshellCausesSleep` on a Mac
//! with a lid, and the time since the user's last input against the
//! system's idle sleep timer.
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;

use super::{Lid, Platform, readings};

/// The longest a reading may take.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct Mac;

pub(super) async fn connect() -> Option<Mac> {
    Some(Mac)
}

impl Platform for Mac {
    async fn lid(&mut self) -> Result<Option<Lid>> {
        let root = read("ioreg", &["-r", "-k", "AppleClamshellState", "-d", "1"]).await?;
        Ok(readings::lid(&root))
    }

    async fn idle(&mut self) -> Result<bool> {
        let Some(timer) = readings::sleep_timer(&read("pmset", &["-g"]).await?) else {
            return Ok(false);
        };
        let input = read("ioreg", &["-r", "-c", "IOHIDSystem", "-d", "1"]).await?;
        let idle = readings::idle_time(&input).context("ioreg lists no HIDIdleTime")?;
        Ok(idle >= timer)
    }
}

/// What `program` run with `args` prints.
async fn read(program: &str, args: &[&str]) -> Result<String> {
    let mut command = Command::new(program);
    command.args(args);
    shoal_subprocess::Run::new(command)
        .timeout(READ_TIMEOUT)
        .output()
        .await
}
