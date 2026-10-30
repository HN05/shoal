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
    // Prepare the complete replacement before touching the user's current file.
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
