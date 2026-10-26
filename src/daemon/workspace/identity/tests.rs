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
fn marker_proves_ownership_before_any_recorded_identity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let git_dir = temp.path();
    let stale = "0:0";
    let current = directory_identity(git_dir)?;
    verify_owner(git_dir, "workspace", Some(&current))?;
    assert!(verify_owner(git_dir, "workspace", Some(stale)).is_err());
    assert!(verify_owner(git_dir, "workspace", None).is_err());
    mark_owner(git_dir, "workspace")?;
    assert_eq!(marked_owner(git_dir)?.as_deref(), Some("workspace"));
    verify_owner(git_dir, "workspace", Some(stale))?;
    let error = verify_owner(git_dir, "other", Some(&current)).unwrap_err();
    assert!(error.to_string().contains("another Shoal workspace"));
    mark_owner(git_dir, "other")?;
    assert_eq!(marked_owner(git_dir)?.as_deref(), Some("other"));
    assert!(!git_dir.join(format!("{OWNER_MARKER}.partial")).exists());
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
