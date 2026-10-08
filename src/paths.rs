//! Locations of Shoal's per-user state. Every file the daemon or CLI touches
//! under the state directory is named here.
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use anyhow::{Context, Result, ensure};

const DEFAULT_STATE: &str = ".local/state/shoal";

/// macOS limits `sun_path`; it is the more restrictive supported platform.
const MAX_SOCKET_PATH_LEN: usize = 104;

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub state: PathBuf,
    pub socket: PathBuf,
}

impl Paths {
    #[cfg(test)]
    pub fn for_test(root: impl AsRef<std::path::Path>) -> Self {
        let home = root.as_ref().to_path_buf();
        let state = home.join("state");
        let socket = state.join("daemon.sock");
        Self {
            home,
            state,
            socket,
        }
    }

    pub fn new(state: Option<PathBuf>) -> Result<Self> {
        let home = crate::fsutil::home_dir()?;
        let state = Self::state_dir(state)?;
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

    /// The state directory `new` selects, without requiring a usable socket path.
    pub fn state_dir(state: Option<PathBuf>) -> Result<PathBuf> {
        let home = crate::fsutil::home_dir()?;
        ensure!(home.is_absolute(), "HOME must be an absolute path");
        let state = state.unwrap_or_else(|| home.join(DEFAULT_STATE));
        Ok(if state.is_absolute() {
            state
        } else {
            std::env::current_dir()?.join(state)
        })
    }

    /// Whether the state directory is the one a CLI with this home uses by default.
    pub fn is_default_state(&self) -> bool {
        self.state == self.home.join(DEFAULT_STATE)
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

    /// Private forge-wrapper PATH directory, held for one tracked agent launch.
    pub fn agent_auth_directory(&self) -> std::io::Result<tempfile::TempDir> {
        tempfile::Builder::new()
            .prefix("agent-auth-")
            .tempdir_in(&self.state)
    }

    /// Run data Shoal keeps for one workspace, such as detached session logs;
    /// deleted with the workspace record.
    pub fn workspace_state(&self, workspace_id: &str) -> PathBuf {
        self.state.join("workspaces").join(workspace_id)
    }
}
