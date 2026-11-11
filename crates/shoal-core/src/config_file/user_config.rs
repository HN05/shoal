//! The user's own config directory, which a development build leaves to the
//! installed Shoal, as it leaves the user's state directory: running agents
//! read their settings from it on every command.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

/// Refuse to write `path`, the global config or a file beside it, when it
/// lies in the user's own config directory, unless this build changes user
/// files. Unit tests, this crate's and those of the crates that use its test
/// support, always refuse it, whatever their environment.
pub fn refuse_user_config(path: &Path) -> Result<()> {
    if !cfg!(any(test, feature = "test-support")) && crate::build::changes_user_files() {
        return Ok(());
    }
    refuse_within(path, &user_config_dirs())
}

/// Refuse `path` when it lies in one of `directories`.
fn refuse_within(path: &Path, directories: &[PathBuf]) -> Result<()> {
    let path = resolved(path);
    for directory in directories {
        ensure!(
            !path.starts_with(resolved(directory)),
            "a development build does not write the user's config directory {}; set HOME to a temporary directory, with XDG_CONFIG_HOME unset or inside it, or set {}=1 to write it",
            directory.display(),
            crate::env::DEV_USER_FILES
        );
    }
    Ok(())
}

/// The user's config directories: `.config/shoal` in the account's home, which
/// comes from the password database, so a temporary `HOME` neither hides nor
/// imitates it, and `$XDG_CONFIG_HOME/shoal` unless it lies in a temporary
/// `HOME`, as a test's does.
fn user_config_dirs() -> Vec<PathBuf> {
    let Some(account) = crate::fsutil::home::account_home() else {
        return Vec::new();
    };
    let temporary_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| resolved(home) != resolved(&account));
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|config| config.is_absolute())
        .filter(|config| {
            !temporary_home
                .as_ref()
                .is_some_and(|home| resolved(config).starts_with(resolved(home)))
        });
    [Some(account.join(".config")), config_home]
        .into_iter()
        .flatten()
        .map(|config| config.join("shoal"))
        .collect()
}

/// `path` through the symlinks of its deepest existing ancestor, so a path
/// not yet created compares like one that exists.
fn resolved(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    for ancestor in path.ancestors() {
        if let Ok(existing) = fs::canonicalize(ancestor) {
            return missing
                .iter()
                .rev()
                .fold(existing, |path, name| path.join(name));
        }
        missing.extend(ancestor.file_name());
    }
    path.to_owned()
}

#[cfg(test)]
mod tests;
