//! The state directory of this build of the `shoal` command. A development
//! build leaves the user's own state directory, whose daemon serves every
//! running agent, to the installed Shoal, as it leaves the user's other files;
//! apps reach that daemon whatever their build.
use std::{
    fs,
    ops::Deref,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

pub(crate) use crate::client::paths::DEFAULT_STATE;
pub use crate::client::paths::{Paths, SESSION_LOG};
use crate::client::{connection::Daemon, scope_tokens::ScopeToken, space_ids::SpaceId};

/// The daemon of the `shoal` command, which its requests reach with the scope
/// token the command was handed or inherited; apps choose theirs with
/// [`crate::client::connection::Connector`] instead.
#[derive(Debug, Clone)]
pub struct CommandPaths(Paths);

impl From<Paths> for CommandPaths {
    fn from(paths: Paths) -> Self {
        Self(paths)
    }
}

impl From<CommandPaths> for Paths {
    fn from(paths: CommandPaths) -> Self {
        paths.0
    }
}

impl Deref for CommandPaths {
    type Target = Paths;

    fn deref(&self) -> &Paths {
        &self.0
    }
}

impl Daemon for CommandPaths {
    fn paths(&self) -> &Paths {
        &self.0
    }

    fn scope(&self) -> Option<ScopeToken> {
        crate::env::scope_token()
    }
}

/// The files and directories the daemon and the `shoal` command keep in the
/// state directory, which apps never read.
pub trait StateFiles {
    /// The state directory.
    fn state(&self) -> &Path;

    /// Lock held by the running daemon; never unlinked so waiters share one inode.
    fn daemon_lock(&self) -> PathBuf {
        self.state().join("daemon.lock")
    }

    fn daemon_log(&self) -> PathBuf {
        self.state().join("daemon.log")
    }

    fn database(&self) -> PathBuf {
        self.state().join("state.db")
    }

    /// The reports agents' hooks keep while they send them, in a directory
    /// per space, where those the daemon did not receive stay for it to find.
    fn hook_reports(&self) -> PathBuf {
        self.state().join("hook-reports")
    }

    /// Copies of the files proposed issues and PRs attach, kept until the
    /// user decides, since the agent's copies may be gone by then.
    fn proposal_files(&self) -> PathBuf {
        self.state().join("proposals")
    }

    /// The workspaces each command last acted on, which `-` selects from.
    fn recent_workspaces(&self) -> PathBuf {
        self.state().join("recent-workspaces.json")
    }

    /// Where a chat started detached keeps its log and first message, named
    /// after the chat's ID, without an extension.
    fn chat_files(&self, chat: &crate::client::model::Chat) -> PathBuf {
        self.state().join("chats").join(chat.id.as_str())
    }

    /// Where each running review keeps its detached checkout and its
    /// reviewer's output, until the review ends.
    fn reviews(&self) -> PathBuf {
        self.state().join("reviews")
    }

    /// The copies of Shoal's release that execution wrappers run from, kept
    /// while one runs from them.
    fn releases(&self) -> PathBuf {
        self.state().join("releases")
    }

    /// Run data Shoal keeps for one space, a workspace or a chat, such as
    /// its agents' recovery records and detached session logs; deleted with
    /// the space's record. The directory keeps the name it had when only
    /// workspaces had one.
    fn space_state(&self, space_id: &SpaceId) -> PathBuf {
        self.state().join("workspaces").join(space_id)
    }
}

impl StateFiles for Paths {
    fn state(&self) -> &Path {
        &self.state
    }
}

/// Operations on the client type [`Paths`] that only the `shoal` command uses.
pub trait PathsExt: Sized {
    /// [`Paths::new`] for this build, refusing the user's state directory
    /// in a development build.
    fn of_command(state: Option<PathBuf>) -> Result<Self>;

    /// [`Paths::state_dir`] for this build, as [`Self::of_command`] refuses it.
    fn command_state_dir(state: Option<PathBuf>) -> Result<PathBuf>;

    /// The push helper's launcher, held for one tracked agent launch.
    fn push_gate_directory(&self) -> std::io::Result<tempfile::TempDir>;

    /// Secret tool launchers, held for one tracked agent launch.
    fn secret_tools_directory(&self) -> std::io::Result<tempfile::TempDir>;
}

impl PathsExt for Paths {
    fn of_command(state: Option<PathBuf>) -> Result<Self> {
        let paths = Self::new(state)?;
        refuse_user_state(&paths.state)?;
        Ok(paths)
    }

    fn command_state_dir(state: Option<PathBuf>) -> Result<PathBuf> {
        let state = Self::state_dir(state)?;
        refuse_user_state(&state)?;
        Ok(state)
    }

    fn push_gate_directory(&self) -> std::io::Result<tempfile::TempDir> {
        tempfile::Builder::new()
            .prefix("push-gate-")
            .tempdir_in(&self.state)
    }

    fn secret_tools_directory(&self) -> std::io::Result<tempfile::TempDir> {
        tempfile::Builder::new()
            .prefix("secret-tools-")
            .tempdir_in(&self.state)
    }
}

/// Refuse the user's state directory in a development build. The user's home
/// comes from the password database, so a temporary `HOME` neither hides nor
/// imitates it.
fn refuse_user_state(state: &Path) -> Result<()> {
    if crate::build::changes_user_files() {
        return Ok(());
    }
    let Some(home) = crate::fsutil::home::account_home() else {
        return Ok(());
    };
    let user = home.join(DEFAULT_STATE);
    ensure!(
        !same_directory(state, &user),
        "a development build does not use the user's state directory {}; pass --state-dir with a temporary directory, or set {}=1 to use it",
        user.display(),
        crate::env::DEV_USER_FILES
    );
    Ok(())
}

/// Whether `a` and `b` name one directory, through symlinks when both exist.
fn same_directory(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (fs::canonicalize(a), fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

#[cfg(test)]
mod tests;
