//! Copies of files and directory trees: mirrors, clones and moves.
use std::{ffi::OsStr, fs, io, path::Path};

use anyhow::Context;

use super::off_runtime;

/// Copy the contents of a worktree without copying its Git administrative
/// file. The destination may already contain the clean checkout created for
/// the copied branch; entries the source lacks are removed at every depth, so
/// deletions in the source stay deleted.
pub fn copy_worktree(source: &Path, destination: &Path) -> io::Result<()> {
    mirror_children(source, destination, |name| name != ".git")
}

/// Make `destination`'s children match `source`'s, considering only names
/// `include` accepts on either side.
pub(crate) fn mirror_children(
    source: &Path,
    destination: &Path,
    include: fn(&OsStr) -> bool,
) -> io::Result<()> {
    remove_entries_missing_from(source, destination, include)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if include(&entry.file_name()) {
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn remove_entries_missing_from(
    source: &Path,
    destination: &Path,
    include: fn(&OsStr) -> bool,
) -> io::Result<()> {
    for entry in fs::read_dir(destination)? {
        let entry = entry?;
        if include(&entry.file_name()) && source.join(entry.file_name()).symlink_metadata().is_err()
        {
            remove_existing(&entry.path())?;
        }
    }
    Ok(())
}

fn copy_entry(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        remove_existing(destination)?;
        std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
    } else if file_type.is_dir() {
        if destination.is_symlink() || (destination.exists() && !destination.is_dir()) {
            remove_existing(destination)?;
        }
        fs::create_dir_all(destination)?;
        mirror_children(source, destination, |_| true)?;
        fs::set_permissions(destination, metadata.permissions())?;
    } else {
        // Copying onto an existing path would write through a symlink or a
        // hard link to a file outside the destination.
        remove_existing(destination)?;
        fs::copy(source, destination)?;
        fs::set_permissions(destination, metadata.permissions())?;
    }
    Ok(())
}

fn remove_existing(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Clone `source`, a file or directory tree, to the missing `destination`
/// without copying data: APFS clones on macOS and reflinks on Linux, keeping
/// modification times. Fails instead of falling back to a full copy, which
/// would cost the disk space cloning saves.
pub async fn clone_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let (from, to) = (source.to_owned(), destination.to_owned());
        tokio::task::spawn_blocking(move || clone_file(&from, &to))
            .await?
            .with_context(|| format!("clone {}", destination.display()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut command = tokio::process::Command::new("cp");
        command
            .args([
                "--reflink=always",
                "--archive",
                "--no-target-directory",
                "--",
            ])
            .arg(source)
            .arg(destination);
        let result = crate::subprocess::output(command).await;
        if result.is_err() {
            let _ = remove_existing(destination);
        }
        result
            .map(drop)
            .with_context(|| format!("clone {}", destination.display()))
    }
}

/// Clone `source`, a file or directory tree, to the missing `destination`
/// as [`clone_tree`] does on macOS, or copy it where it cannot be cloned, as
/// on another volume. A failed copy removes what it wrote.
pub(crate) fn clone_or_copy(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if clone_file(source, destination).is_ok() {
        return Ok(());
    }
    let copied = copy_entry(source, destination);
    if copied.is_err() {
        let _ = remove_existing(destination);
    }
    copied
}

/// clonefile(2) of `source` to the missing `destination`, which clones a
/// directory with everything in it.
#[cfg(target_os = "macos")]
fn clone_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    // clone(2) flag: clone a symlink itself instead of its target.
    const CLONE_NOFOLLOW: u32 = 0x0001;
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let target = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
    // SAFETY: both arguments are NUL-terminated paths that outlive the call.
    let status = unsafe { libc::clonefile(source.as_ptr(), target.as_ptr(), CLONE_NOFOLLOW) };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Give the existing directory `destination` each of `source`'s children
/// except its `.git` file, cloned as [`clone_tree`] clones them, or copied
/// where the filesystem cannot clone.
pub(crate) async fn clone_worktree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let from = source.to_owned();
    let names = off_runtime::run(move || {
        let mut names = Vec::new();
        for entry in fs::read_dir(&from)? {
            names.push(entry?.file_name());
        }
        Ok(names)
    })
    .await?;
    for name in names.into_iter().filter(|name| name != ".git") {
        let (from, to) = (source.join(&name), destination.join(&name));
        if clone_tree(&from, &to).await.is_ok() {
            continue;
        }
        off_runtime::run(move || {
            remove_existing(&to)?;
            copy_entry(&from, &to).with_context(|| format!("copy {}", to.display()))
        })
        .await?;
    }
    Ok(())
}

/// How [`move_directory`] moved a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    Renamed,
    /// Copied to another filesystem; the source is still there.
    Copied,
}

/// Move the directory `source` onto the empty directory `destination`: a
/// rename on one filesystem, otherwise a copy that keeps symlinks, modes and
/// times and leaves `source` for the caller to delete once it no longer needs
/// it. A failed copy leaves `destination` partly filled.
pub async fn move_directory(source: &Path, destination: &Path) -> anyhow::Result<Moved> {
    match fs::rename(source, destination) {
        Ok(()) => return Ok(Moved::Renamed),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("move {} to {}", source.display(), destination.display())
            });
        }
    }
    copy_directory(source, destination).await?;
    Ok(Moved::Copied)
}

/// Copy the entries of `source` into `destination`, leaving the destination
/// directory's own attributes, and with them its identity, unchanged.
pub async fn copy_directory(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let entries = fs::read_dir(source)
        .and_then(|entries| {
            entries
                .map(|entry| Ok(entry?.path()))
                .collect::<io::Result<Vec<_>>>()
        })
        .with_context(|| format!("read {}", source.display()))?;
    if entries.is_empty() {
        return Ok(());
    }
    copy_keeping_attributes(&entries, destination)
        .await
        .with_context(|| format!("copy {} to {}", source.display(), destination.display()))
}

/// Clone `source`, a file or directory tree, to `destination` as
/// [`clone_tree`] does, or where it cannot be cloned copy it with `cp`,
/// which keeps modification times too, unlike [`clone_or_copy`]. A failed
/// copy leaves what it wrote.
pub async fn clone_or_copy_keeping_times(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if clone_tree(source, destination).await.is_ok() {
        return Ok(());
    }
    copy_keeping_attributes(&[source], destination)
        .await
        .with_context(|| format!("copy {}", source.display()))
}

/// `cp -pPR` of `sources` to `destination`: into it when it is a directory,
/// otherwise as it for a single source.
async fn copy_keeping_attributes(
    sources: &[impl AsRef<OsStr>],
    destination: &Path,
) -> anyhow::Result<()> {
    let mut command = tokio::process::Command::new("cp");
    command.args(["-pPR", "--"]).args(sources).arg(destination);
    crate::subprocess::output(command).await.map(drop)
}

#[cfg(test)]
mod tests;
