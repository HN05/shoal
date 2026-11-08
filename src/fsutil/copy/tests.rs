use super::*;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn copying_a_worktree_replaces_destination_links_instead_of_following_them() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    let outside = temp.path().join("outside");
    fs::write(&outside, "unrelated")?;
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600))?;
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    fs::create_dir_all(&source)?;
    fs::create_dir_all(&destination)?;
    // The source replaced tracked links with regular files; the clean
    // checkout still has the links, one absolute and one hard.
    for name in ["symlink", "hardlink"] {
        fs::write(source.join(name), "worktree")?;
        fs::set_permissions(source.join(name), fs::Permissions::from_mode(0o755))?;
    }
    std::os::unix::fs::symlink(&outside, destination.join("symlink"))?;
    fs::hard_link(&outside, destination.join("hardlink"))?;

    copy_worktree(&source, &destination)?;

    assert_eq!(fs::read_to_string(&outside)?, "unrelated");
    assert_eq!(fs::metadata(&outside)?.permissions().mode() & 0o777, 0o600);
    for name in ["symlink", "hardlink"] {
        let copied = destination.join(name);
        assert!(fs::symlink_metadata(&copied)?.file_type().is_file());
        assert_eq!(fs::read_to_string(&copied)?, "worktree");
        assert_eq!(fs::metadata(&copied)?.permissions().mode() & 0o777, 0o755);
    }
    Ok(())
}

#[tokio::test]
async fn moving_a_directory_keeps_its_contents_and_links() -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("nested"))?;
    fs::write(source.join("nested/file"), "contents")?;
    fs::set_permissions(
        source.join("nested/file"),
        fs::Permissions::from_mode(0o750),
    )?;
    std::os::unix::fs::symlink("nested/file", source.join("link"))?;

    let copied = temp.path().join("copied");
    fs::create_dir(&copied)?;
    let before = fs::metadata(&copied)?;
    copy_directory(&source, &copied).await?;
    // The destination itself keeps its identity.
    let after = fs::metadata(&copied)?;
    assert_eq!(after.ino(), before.ino());
    assert_eq!(after.created().ok(), before.created().ok());
    assert_eq!(fs::read_to_string(copied.join("nested/file"))?, "contents");
    assert_eq!(
        fs::metadata(copied.join("nested/file"))?
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(
        fs::read_link(copied.join("link"))?,
        Path::new("nested/file")
    );
    assert!(source.join("nested/file").exists());

    let renamed = temp.path().join("renamed");
    fs::create_dir(&renamed)?;
    assert_eq!(move_directory(&source, &renamed).await?, Moved::Renamed);
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(renamed.join("nested/file"))?, "contents");
    Ok(())
}

#[test]
fn copying_a_worktree_removes_entries_deleted_at_any_depth() -> io::Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("nested"))?;
    fs::write(source.join(".git"), "gitdir: source")?;
    fs::write(source.join("nested/kept"), "changed")?;

    // The clean checkout still has what the source deleted.
    let destination = temp.path().join("destination");
    fs::create_dir_all(destination.join("nested/gone/deeper"))?;
    fs::write(destination.join(".git"), "gitdir: destination")?;
    fs::write(destination.join("removed"), "old")?;
    fs::write(destination.join("nested/kept"), "old")?;
    fs::write(destination.join("nested/deleted"), "old")?;
    fs::write(destination.join("nested/gone/deeper/file"), "old")?;

    copy_worktree(&source, &destination)?;
    assert_eq!(
        fs::read_to_string(destination.join(".git"))?,
        "gitdir: destination"
    );
    assert_eq!(
        fs::read_to_string(destination.join("nested/kept"))?,
        "changed"
    );
    assert!(!destination.join("removed").exists());
    assert!(!destination.join("nested/deleted").exists());
    assert!(!destination.join("nested/gone").exists());
    Ok(())
}
