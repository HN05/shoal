//! The config files on disk: edits hold a lock, check the whole result
//! before saving and keep the previous file as a backup. A development build
//! leaves the user's own config directory to the installed Shoal.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

use crate::fsutil::{
    self,
    atomic::{Permissions, ReplaceOptions},
};

use crate::config::{
    Config, PACKAGED,
    edit::{self, Change},
};
use crate::paths::Paths;

mod user_config;

pub use user_config::refuse_user_config;

/// Apply `changes` to the file at `path` and save the result once `check`
/// accepts it.
pub fn edit(
    path: PathBuf,
    changes: &[Change],
    check: impl FnOnce(&str) -> Result<()>,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let _lock = lock(&path)?;
    let text = fsutil::read_optional(&path)
        .with_context(|| format!("read {}", path.display()))?
        .unwrap_or_default();
    let edited = edit::apply(&text, changes)?;
    check(&edited)?;
    replace(path, &edited)
}

/// Hold `path`'s edit lock while the returned file lives.
pub(crate) fn lock(path: &Path) -> Result<fs::File> {
    refuse_user_config(path)?;
    fs::create_dir_all(path.parent().context("config path has no parent")?)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("toml.lock"))?;
    fs2::FileExt::lock_exclusive(&file)?;
    Ok(file)
}

/// Replace `path` with `text`, keeping the previous file as `<path>.backup`.
/// Returns the path and the backup, if one was made.
pub(crate) fn replace(path: PathBuf, text: &str) -> Result<(PathBuf, Option<PathBuf>)> {
    refuse_user_config(&path)?;
    let parent = path.parent().context("config path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    // Prepare the complete replacement before touching the user's current file.
    let temporary = fsutil::atomic::prepare_atomic_write(
        &path,
        text.as_bytes(),
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: false,
        },
    )?;
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        ensure!(
            !metadata.is_dir(),
            "config is a directory: {}",
            path.display()
        );
    }
    // The current entry stays in place until the replacement takes its name
    // at once, so a reader never finds it missing.
    let backup = path.with_extension("toml.backup");
    let kept = keep_backup(&path, &backup)
        .with_context(|| format!("keep {} as {}", path.display(), backup.display()))?;
    temporary
        .persist(&path)
        .with_context(|| format!("install config at {}", path.display()))?;
    Ok((path, kept.then_some(backup)))
}

/// Leave a copy of `path` at `backup`, replacing whatever is there without
/// following it: a symlink is kept as a symlink to the same target. Whether
/// there was a `path` to keep.
fn keep_backup(path: &Path, backup: &Path) -> std::io::Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match fs::remove_file(backup) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    if metadata.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(path)?, backup)?;
    } else {
        fs::copy(path, backup)?;
    }
    Ok(true)
}

/// Write `packaged` at `path` unless a file already exists there. Whether
/// this call created it.
fn install_at(path: &Path, packaged: &str) -> Result<bool> {
    refuse_user_config(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            std::io::Write::write_all(&mut file, packaged.as_bytes())
                .with_context(|| format!("write {}", path.display()))?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error).with_context(|| format!("create {}", path.display())),
    }
}

/// The packaged config named `name`.
fn packaged(name: &str) -> Result<&'static str> {
    PACKAGED
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, text)| *text)
        .with_context(|| format!("unknown packaged config: {name}"))
}

/// The global config's writes; those that replace a file hold its edit lock.
pub trait ConfigExt: Sized {
    /// Write the commented template unless a config file already exists.
    /// Returns the path and whether this call created it.
    fn install(paths: &Paths) -> Result<(PathBuf, bool)>;

    /// Write a fresh template, first moving any existing file to
    /// `config.toml.backup` (replacing an older backup). Returns the path and
    /// the backup, if one was made.
    fn reset(paths: &Paths) -> Result<(PathBuf, Option<PathBuf>)>;

    /// Apply `changes` together, saving only when the result is valid.
    fn edit(paths: &Paths, changes: &[edit::Change]) -> Result<(PathBuf, Option<PathBuf>)>;

    fn install_named(paths: &Paths, name: &str) -> Result<(PathBuf, Option<PathBuf>)>;

    /// [`Self::load`] under the edit lock, so a concurrent edit is read whole
    /// rather than while its file is moved aside. A build that may not write
    /// the file reads it without the lock, whose file it would create there.
    fn load_settled(paths: &Paths) -> Result<Self>;
}

impl ConfigExt for Config {
    fn install(paths: &Paths) -> Result<(PathBuf, bool)> {
        let path = Self::path(paths);
        let created = install_at(&path, packaged("default")?)?;
        Ok((path, created))
    }

    fn reset(paths: &Paths) -> Result<(PathBuf, Option<PathBuf>)> {
        Self::install_named(paths, "default")
    }

    fn edit(paths: &Paths, changes: &[edit::Change]) -> Result<(PathBuf, Option<PathBuf>)> {
        edit(Self::path(paths), changes, |edited| {
            Self::parse(edited, paths)
                .map(drop)
                .context("invalid edited config")
        })
    }

    fn install_named(paths: &Paths, name: &str) -> Result<(PathBuf, Option<PathBuf>)> {
        let text = packaged(name)?;
        Self::parse(text, paths).with_context(|| format!("validate packaged config {name}"))?;
        let path = Self::path(paths);
        let _lock = lock(&path)?;
        replace(path, text)
    }

    fn load_settled(paths: &Paths) -> Result<Self> {
        let path = Self::path(paths);
        let _lock = refuse_user_config(&path)
            .is_ok()
            .then(|| lock(&path))
            .transpose()?;
        Self::load(paths)
    }
}

#[cfg(test)]
mod tests;
