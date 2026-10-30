//! The config files on disk: edits hold a lock, check the whole result
//! before saving and keep the previous file as a backup.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

use crate::fsutil::{self, Permissions, ReplaceOptions};

use super::edit::{self, Change};

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
pub fn lock(path: &Path) -> Result<fs::File> {
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
pub fn replace(path: PathBuf, text: &str) -> Result<(PathBuf, Option<PathBuf>)> {
    let parent = path.parent().context("config path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    // Prepare the complete replacement before moving the user's current file.
    let temporary = fsutil::prepare_atomic_write(
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
    let backup = path.with_extension("toml.backup");
    let moved = match fs::rename(&path, &backup) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("move {} to {}", path.display(), backup.display()));
        }
    };
    temporary
        .persist_noclobber(&path)
        .with_context(|| format!("install config at {}", path.display()))?;
    Ok((path, moved.then_some(backup)))
}
