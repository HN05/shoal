//! The state directory of this build of the `shoal` command. A development
//! build leaves the user's own state directory, whose daemon serves every
//! running agent, to the installed Shoal, as it leaves the user's other files;
//! apps reach that daemon whatever their build.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

pub(crate) use crate::client::paths::{DEFAULT_STATE, Paths, SESSION_LOG, SESSION_PROMPT};

/// Operations on the client type [`Paths`] that only the `shoal` command uses.
pub(crate) trait PathsExt: Sized {
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
    let Some(home) = crate::fsutil::account_home() else {
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
