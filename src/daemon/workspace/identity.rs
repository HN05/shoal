//! Persistent identity for directories Shoal owns: Git administrative
//! directories and repository checkouts.
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// Names the owning workspace inside a worktree's Git admin directory. Git keeps
/// unknown files there across moves and discards them with the worktree, so the
/// marker survives device renumbering, restores and copies but not re-creation.
const OWNER_MARKER: &str = "shoal-workspace";

/// The workspace ID recorded in an admin directory, if Shoal marked it.
pub(in crate::daemon) fn marked_owner(git_dir: &Path) -> Result<Option<String>> {
    match fs::read_to_string(git_dir.join(OWNER_MARKER)) {
        Ok(owner) => Ok(Some(owner.trim_end().to_owned())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("read Git worktree owner marker"),
    }
}

pub(in crate::daemon) fn mark_owner(git_dir: &Path, workspace_id: &str) -> Result<()> {
    // Rename into place so a crash never leaves a truncated marker.
    let partial = git_dir.join(format!("{OWNER_MARKER}.partial"));
    fs::write(&partial, format!("{workspace_id}\n")).context("write Git worktree owner marker")?;
    fs::rename(&partial, git_dir.join(OWNER_MARKER)).context("write Git worktree owner marker")
}

pub(super) fn clear_owner(git_dir: &Path) -> Result<()> {
    match fs::remove_file(git_dir.join(OWNER_MARKER)) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).context("remove Git worktree owner marker")
        }
        _ => Ok(()),
    }
}

/// Prove an admin directory belongs to a workspace: by its marker, or for records
/// made before markers existed, by the recorded filesystem identity.
pub(super) fn verify_owner(
    git_dir: &Path,
    workspace_id: &str,
    recorded: Option<&str>,
) -> Result<()> {
    match marked_owner(git_dir)? {
        Some(owner) => ensure!(
            owner == workspace_id,
            "Git worktree metadata is marked as another Shoal workspace's; ownership cannot be verified"
        ),
        None => {
            let identity = recorded.context(
                "Git worktree ownership is unverified; check the worktree, then reclaim it with shoal work adopt <its path>",
            )?;
            verify_directory_identity(git_dir, identity)?;
        }
    }
    Ok(())
}

pub(super) fn directory_identity(path: &Path) -> Result<String> {
    let (metadata, birth) = directory_metadata(path)?;
    Ok(format_identity(metadata.dev(), metadata.ino(), birth))
}

/// Whether a directory still has a recorded identity, including the legacy
/// `device:inode` form.
pub(super) fn has_directory_identity(path: &Path, recorded: &str) -> Result<bool> {
    let (metadata, birth) = directory_metadata(path)?;
    Ok(matches_identity(
        recorded,
        metadata.dev(),
        metadata.ino(),
        birth,
    ))
}

fn verify_directory_identity(path: &Path, recorded: &str) -> Result<()> {
    ensure!(
        has_directory_identity(path, recorded)?,
        "Git worktree metadata was replaced or its recorded filesystem identity changed; ownership cannot be verified. Check the worktree, then reclaim it with shoal work adopt <its path>"
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn directory_metadata(path: &Path) -> Result<(fs::Metadata, Option<SystemTime>)> {
    let metadata = fs::metadata(path)?;
    ensure!(metadata.is_dir(), "Git metadata is not a directory");
    let birth = metadata.created().ok();
    Ok((metadata, birth))
}

#[cfg(target_os = "linux")]
fn directory_metadata(path: &Path) -> Result<(fs::Metadata, Option<SystemTime>)> {
    use std::os::unix::fs::OpenOptionsExt;

    // Rust's musl Metadata::created is unsupported even when the filesystem
    // exposes birth time. Pin the directory so stat and statx inspect one inode.
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open(path)?;
    let metadata = directory.metadata()?;
    let birth = linux_birth_time(&directory)?;
    Ok((metadata, birth))
}

#[cfg(target_os = "linux")]
fn linux_birth_time(directory: &fs::File) -> Result<Option<SystemTime>> {
    use rustix::fs::{AtFlags, StatxFlags, statx};
    use rustix::io::Errno;
    use std::time::Duration;

    let stat = match statx(directory, "", AtFlags::EMPTY_PATH, StatxFlags::BTIME) {
        Ok(stat) => stat,
        Err(Errno::NOSYS | Errno::OPNOTSUPP) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if stat.stx_mask & StatxFlags::BTIME.bits() == 0 || stat.stx_btime.tv_sec < 0 {
        return Ok(None);
    }
    let birth = Duration::new(stat.stx_btime.tv_sec as u64, stat.stx_btime.tv_nsec);
    Ok(UNIX_EPOCH.checked_add(birth))
}

fn matches_identity(recorded: &str, device: u64, inode: u64, birth: Option<SystemTime>) -> bool {
    recorded == format_identity(device, inode, birth) || recorded == format!("{device}:{inode}")
}

fn format_identity(device: u64, inode: u64, birth: Option<SystemTime>) -> String {
    // Device numbers can change across mounts (notably Btrfs). Birth time
    // survives those changes and distinguishes reuse of a directory inode.
    match birth.and_then(|time| time.duration_since(UNIX_EPOCH).ok()) {
        Some(time) => format!("birth:{inode}:{}:{}", time.as_secs(), time.subsec_nanos()),
        None => format!("{device}:{inode}"),
    }
}

#[cfg(test)]
mod tests;
