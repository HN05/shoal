//! Locations of Shoal's per-user state. Every file the daemon or CLI touches
//! under the state directory is named here.
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use anyhow::{Context, Result, ensure};

/// macOS limits `sun_path`; it is the more restrictive supported platform.
const MAX_SOCKET_PATH_LEN: usize = 104;

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
        ensure!(
            socket.as_os_str().len() < MAX_SOCKET_PATH_LEN,
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

    /// Lock held by the running daemon; never unlinked so waiters share one inode.
    pub fn daemon_lock(&self) -> PathBuf {
        self.state.join("daemon.lock")
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.state.join("daemon.log")
    }

    pub fn database(&self) -> PathBuf {
        self.state.join("state.db")
    }

    /// Managed Worktrunk configuration, isolating Shoal from personal hooks.
    pub fn worktrunk_config(&self) -> PathBuf {
        self.state.join("worktrunk.toml")
    }

    /// Run data Shoal keeps for one workspace, such as detached session logs;
    /// deleted with the workspace record.
    pub fn workspace_state(&self, workspace_id: &str) -> PathBuf {
        self.state.join("workspaces").join(workspace_id)
    }
}
