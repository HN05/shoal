//! Packages of spaces that an earlier release's `shoal move ssh` kept in
//! the state directory after their import on another machine failed, which
//! the daemon moves out of it once, since the state directory holds only
//! its own state.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use crate::{
    client::notifications::NotificationKind,
    daemon::{log, workspace::Manager},
    fsutil::off_runtime,
};

/// The directory an earlier release kept them in, in the state directory,
/// and the one in `root_dir` they move to.
const UNMOVED: &str = "unmoved";

impl Manager {
    /// Move the packages an earlier `shoal move ssh` kept in
    /// `<state>/unmoved` to `<root_dir>/unmoved/`, then record one
    /// notification naming each with the `shoal move import` that continues
    /// it. They may be the only copy of a chat, so nothing deletes them, and
    /// one that cannot move stays for the next start.
    pub(crate) async fn move_unmoved_packages(&self) -> Result<()> {
        let kept = self.paths.state.join(UNMOVED);
        if !off_runtime::exists(&kept).await? {
            return Ok(());
        }
        let target = self.config().root_dir(&self.paths)?.join(UNMOVED);
        let moved = off_runtime::run(move || move_packages(&kept, &target)).await?;
        if !moved.is_empty() {
            (self.notify(None, NotificationKind::UnmovedPackages, notice(&moved))).await;
        }
        Ok(())
    }
}

/// Move each file in `kept` into `target` under its name, then remove
/// `kept` once empty, returning where each went.
fn move_packages(kept: &Path, target: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(target)
        .with_context(|| format!("create the directory {}", target.display()))?;
    let entries = std::fs::read_dir(kept)
        .with_context(|| format!("read the directory {}", kept.display()))?;
    let mut moved = Vec::new();
    for entry in entries {
        let package = entry?.path();
        let destination = target.join(package.file_name().context("a package has no name")?);
        match move_package(&package, &destination) {
            Ok(()) => moved.push(destination),
            Err(error) => log!("package {} not moved: {error:#}", package.display()),
        }
    }
    if let Err(error) = std::fs::remove_dir(kept) {
        log!("{} not removed: {error}", kept.display());
    }
    Ok(moved)
}

/// Move the file `from` to `to`, which must not exist, copying it whole
/// before removing it where renaming cannot, as across file systems.
fn move_package(from: &Path, to: &Path) -> Result<()> {
    ensure!(!to.exists(), "{} exists", to.display());
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let directory = to
        .parent()
        .context("a package's destination has no directory")?;
    let mut copy = tempfile::NamedTempFile::new_in(directory)?;
    std::io::copy(&mut std::fs::File::open(from)?, copy.as_file_mut())?;
    copy.persist_noclobber(to)?;
    std::fs::remove_file(from)?;
    Ok(())
}

/// The notification of the packages `moved` names, each with the command
/// that continues its space here.
fn notice(moved: &[PathBuf]) -> String {
    let mut notice = String::from(
        "An earlier shoal move ssh kept these packages of spaces whose import failed; each continues here with:",
    );
    for package in moved {
        let package = package.to_string_lossy();
        let command = crate::shell::quote(&["shoal", "move", "import", package.as_ref()]);
        notice.push_str(&format!("\n  {command}"));
    }
    notice
}

#[cfg(test)]
mod tests;
