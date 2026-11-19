//! A directory's filesystem identity as version 208 named it: its birth
//! time where the filesystem records one, else its device and inode.
use anyhow::Result;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// The identity of the directory at `path`, which a test of the migration
/// expects.
#[cfg(test)]
pub(crate) fn directory_identity(path: &Path) -> Result<String> {
    let (metadata, birth) = directory_metadata(path)?;
    Ok(format_identity(metadata.dev(), metadata.ino(), birth))
}

/// The identity of the directory at `path`, when `recorded` is the
/// `device:inode` an earlier version recorded for it where its birth time
/// now names it.
pub(crate) fn birth_identity(path: &Path, recorded: &str) -> Option<String> {
    let (metadata, birth) = directory_metadata(path).ok()?;
    let identity = format_identity(metadata.dev(), metadata.ino(), birth);
    (recorded == format!("{}:{}", metadata.dev(), metadata.ino()) && identity != recorded)
        .then_some(identity)
}

#[cfg(not(target_os = "linux"))]
fn directory_metadata(path: &Path) -> Result<(fs::Metadata, Option<SystemTime>)> {
    let metadata = fs::metadata(path)?;
    anyhow::ensure!(metadata.is_dir(), "Git metadata is not a directory");
    let birth = metadata.created().ok();
    Ok((metadata, birth))
}

#[cfg(target_os = "linux")]
fn directory_metadata(path: &Path) -> Result<(fs::Metadata, Option<SystemTime>)> {
    use std::os::unix::fs::OpenOptionsExt;

    // Musl's Metadata::created is unsupported even where the filesystem
    // records birth time; statx reads it from the pinned directory.
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

fn format_identity(device: u64, inode: u64, birth: Option<SystemTime>) -> String {
    match birth.and_then(|time| time.duration_since(UNIX_EPOCH).ok()) {
        Some(time) => format!("birth:{inode}:{}:{}", time.as_secs(), time.subsec_nanos()),
        None => format!("{device}:{inode}"),
    }
}
