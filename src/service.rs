//! Install, start and stop the daemon through its per-user service manager.
use crate::{paths::Paths, tools::Tool};
use anyhow::{Result, bail, ensure};
use std::path::Path;

mod control;
mod definition;
use control::{command, domain, stop_command};
use definition::{check_state_dir, text, write_definition};
pub use definition::{definition, executable, file};

const LABEL: &str = "com.shoal.daemon";
const UNIT: &str = "shoal.service";

#[derive(Clone, Copy)]
pub enum Platform {
    Mac,
    Linux,
}

impl Platform {
    pub fn current() -> Result<Self> {
        if cfg!(target_os = "macos") {
            Ok(Self::Mac)
        } else if cfg!(target_os = "linux") {
            Ok(Self::Linux)
        } else {
            bail!("Shoal supports macOS and Linux")
        }
    }
}

pub async fn setup(paths: &Paths, executable: &Path, preserve_running: bool) -> Result<()> {
    let platform = Platform::current()?;
    let definition = definition(paths, executable, platform)?;
    paths.prepare()?;
    let changed = write_definition(&definition)?;
    match platform {
        Platform::Mac => {
            let target = format!("{}/{LABEL}", domain());
            if changed
                && !preserve_running
                && command(Tool::Launchctl, &["print", &target], false).await?
            {
                stop_command(Tool::Launchctl, &["bootout", &target]).await?;
            }
        }
        Platform::Linux => {
            command(Tool::Systemctl, &["--user", "daemon-reload"], true).await?;
            command(Tool::Systemctl, &["--user", "enable", UNIT], true).await?;
            if changed && !preserve_running {
                stop_command(Tool::Systemctl, &["--user", "restart", UNIT]).await?;
            }
        }
    }
    start(paths).await
}

pub async fn start(paths: &Paths) -> Result<()> {
    let platform = Platform::current()?;
    let path = file(paths, platform);
    ensure!(
        path.is_file(),
        "daemon service is not installed; run `shoal daemon install`"
    );
    check_state_dir(paths, &path, platform)?;
    match platform {
        Platform::Mac => {
            let domain = domain();
            let target = format!("{domain}/{LABEL}");
            command(Tool::Launchctl, &["enable", &target], true).await?;
            if !command(Tool::Launchctl, &["print", &target], false).await? {
                command(Tool::Launchctl, &["bootstrap", &domain, text(&path)?], true).await?;
            }
            command(Tool::Launchctl, &["kickstart", &target], true).await?;
        }
        Platform::Linux => {
            command(Tool::Systemctl, &["--user", "start", UNIT], true).await?;
        }
    }
    Ok(())
}

pub async fn stop(paths: &Paths) -> Result<()> {
    let platform = Platform::current()?;
    let path = file(paths, platform);
    if !path.exists() {
        return Ok(());
    }
    check_state_dir(paths, &path, platform)?;
    match platform {
        Platform::Mac => {
            let target = format!("{}/{LABEL}", domain());
            if command(Tool::Launchctl, &["print", &target], false).await? {
                stop_command(Tool::Launchctl, &["bootout", &target]).await?;
            }
        }
        Platform::Linux => {
            if file(paths, Platform::Linux).exists() {
                stop_command(Tool::Systemctl, &["--user", "stop", UNIT]).await?;
            }
        }
    }
    Ok(())
}

/// Whether the OS service runs the daemon of this state directory.
pub fn installed(paths: &Paths) -> bool {
    Platform::current().is_ok_and(|platform| {
        let path = file(paths, platform);
        path.is_file() && check_state_dir(paths, &path, platform).is_ok()
    })
}
