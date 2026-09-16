use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use anyhow::{Context, Result, ensure};

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub state: PathBuf,
    pub socket: PathBuf,
}

impl Paths {
    pub fn new(state: Option<PathBuf>) -> Result<Self> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        ensure!(home.is_absolute(), "HOME must be an absolute path");
        let state = state.unwrap_or_else(|| home.join(".local/state/shoal"));
        let state = if state.is_absolute() {
            state
        } else {
            std::env::current_dir()?.join(state)
        };
        let socket = state.join("daemon.sock");
        // macOS is the more restrictive of the two supported platforms.
        ensure!(
            socket.as_os_str().len() < 104,
            "socket path is too long: {}; use a shorter --state-dir",
            socket.display()
        );
        Ok(Self {
            home,
            state,
            socket,
        })
    }

    pub fn prepare(&self) -> Result<()> {
        fs::create_dir_all(&self.state).context("create state directory")?;
        fs::set_permissions(&self.state, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }
}
