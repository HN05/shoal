//! Persistent identity for Git administrative directories.
use anyhow::{Result, ensure};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) fn directory_identity(path: &Path) -> Result<String> {
    let (metadata, birth) = directory_metadata(path)?;
    Ok(format_identity(metadata.dev(), metadata.ino(), birth))
}

pub(super) fn verify_directory_identity(path: &Path, recorded: &str) -> Result<()> {
    let (metadata, birth) = directory_metadata(path)?;
    ensure!(
        matches_identity(recorded, metadata.dev(), metadata.ino(), birth),
        "Git worktree metadata was replaced or its recorded filesystem identity changed; ownership cannot be verified"
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

pub(in crate::daemon) fn needs_identity_upgrade(recorded: Option<&str>) -> bool {
    recorded.is_none_or(|identity| !identity.starts_with("birth:"))
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
mod tests {
    use super::*;
    use std::time::Duration;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_identity_uses_birth_time_when_the_filesystem_exposes_it() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let output = std::process::Command::new("stat")
            .args(["-c", "%W"])
            .arg(temp.path())
            .output()?;
        ensure!(output.status.success(), "stat failed");
        let seconds: u64 = std::str::from_utf8(&output.stdout)?.trim().parse()?;
        if seconds != 0 {
            let identity = directory_identity(temp.path())?;
            assert!(identity.starts_with("birth:"), "{identity}");
            assert_eq!(
                identity.split(':').nth(2),
                Some(seconds.to_string().as_str())
            );
        }
        Ok(())
    }

    #[test]
    fn identity_survives_device_changes_but_not_inode_reuse() {
        let birth = Some(UNIX_EPOCH + Duration::new(100, 42));
        let original = format_identity(55, 123, birth);
        assert_eq!(format_identity(56, 123, birth), original);
        assert_ne!(format_identity(56, 124, birth), original);
        let later = Some(UNIX_EPOCH + Duration::new(100, 43));
        assert_ne!(format_identity(56, 123, later), original);
        assert_ne!(
            format_identity(55, 123, None),
            format_identity(56, 123, None)
        );
    }

    #[test]
    fn legacy_identity_requires_both_device_and_inode_before_upgrade() {
        let birth = Some(UNIX_EPOCH + Duration::new(100, 42));
        assert!(matches_identity("55:123", 55, 123, birth));
        assert!(matches_identity("55:123", 55, 123, None));
        assert!(!matches_identity("55:123", 56, 123, birth));
        assert!(!matches_identity("55:123", 55, 124, birth));
        assert!(!matches_identity("invalid", 55, 123, birth));
        let upgraded = format_identity(55, 123, birth);
        assert!(matches_identity(&upgraded, 56, 123, birth));
        assert!(!matches_identity(&upgraded, 55, 123, None));
        assert!(!matches_identity(
            &upgraded,
            55,
            123,
            Some(UNIX_EPOCH + Duration::new(100, 43))
        ));
    }

    #[test]
    fn identity_follows_symlinks_and_renames_but_rejects_replacement() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let original = temp.path().join("original");
        let moved = temp.path().join("moved");
        let alias = temp.path().join("alias");
        fs::create_dir(&original)?;
        let identity = directory_identity(&original)?;
        fs::rename(&original, &moved)?;
        std::os::unix::fs::symlink(&moved, &alias)?;
        assert_eq!(directory_identity(&alias)?, identity);
        fs::create_dir(&original)?;
        assert_ne!(directory_identity(&original)?, identity);
        fs::write(temp.path().join("file"), "data")?;
        assert!(directory_identity(&temp.path().join("file")).is_err());
        assert!(directory_identity(&temp.path().join("missing")).is_err());
        Ok(())
    }
}
