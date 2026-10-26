use super::*;

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

#[test]
fn marked_directories_are_never_found_unmarked_or_claimed() {
    let temp = tempfile::tempdir().unwrap();
    for attempt in 0..20 {
        let root = temp.path().join(format!("repo-{attempt}/.caches"));
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    start.wait();
                    create_marked_dir(&root, "repo").unwrap();
                    assert!(is_marked_dir(&root, "repo"));
                });
            }
        });
    }
    // A directory someone else created is never claimed.
    let theirs = temp.path().join("theirs");
    fs::create_dir(&theirs).unwrap();
    create_marked_dir(&theirs, "repo").unwrap();
    assert!(!is_marked_dir(&theirs, "repo"));
    assert_eq!(fs::read_dir(&theirs).unwrap().count(), 0);
}

#[test]
fn file_identity_changes_with_a_new_file_or_link_target() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    let link = temp.path().join("shoal");
    fs::write(&first, "old").unwrap();
    fs::write(&second, "new binary").unwrap();
    std::os::unix::fs::symlink(&first, &link).unwrap();
    let original = FileIdentity::of(&link).unwrap();
    assert!(!original.replaced_at(&link));

    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&second, &link).unwrap();
    assert!(original.replaced_at(&link));

    let original = FileIdentity::of(&first).unwrap();
    fs::rename(&second, &first).unwrap();
    assert!(original.replaced_at(&first));
    fs::remove_file(&first).unwrap();
    assert!(
        !original.replaced_at(&first),
        "a missing file is no replacement"
    );
}

#[test]
fn invoked_path_keeps_an_installation_symlink_to_the_running_binary() {
    let root = tempfile::tempdir().unwrap();
    let keg = root.path().join("keg");
    let bin = root.path().join("bin");
    fs::create_dir_all(&keg).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let binary = keg.join("shoal");
    let other = keg.join("other");
    for file in [&binary, &other] {
        fs::write(file, "").unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let link = bin.join("shoal");
    std::os::unix::fs::symlink(&binary, &link).unwrap();
    let resolve = |arg: &str| {
        invoked_path(
            Some(arg.into()),
            bin.as_os_str(),
            root.path(),
            binary.clone(),
        )
    };
    assert_eq!(resolve(link.to_str().unwrap()), link);
    assert_eq!(resolve("shoal"), link, "found on PATH");
    assert_eq!(
        resolve("bin/shoal"),
        link,
        "relative to the current directory"
    );
    assert_eq!(
        invoked_path(
            Some("shoal".into()),
            "bin".as_ref(),
            root.path(),
            binary.clone()
        ),
        link,
        "found through a relative PATH entry"
    );
    assert_eq!(resolve(other.to_str().unwrap()), binary);
    assert_eq!(resolve("missing"), binary);
    assert_eq!(
        invoked_path(None, bin.as_os_str(), root.path(), binary.clone()),
        binary
    );
}

#[test]
fn available_bytes_reads_the_filesystem_or_fails() {
    let root = tempfile::tempdir().unwrap();
    assert!(available_bytes(root.path()).unwrap() > 0);
    assert!(available_bytes(&root.path().join("missing")).is_err());
}
use std::os::unix::{ffi::OsStrExt, fs::symlink};

#[test]
fn atomic_replace_applies_permissions_and_replaces_symlinks() {
    use std::io::Read;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let path = root.path().join("destination");
    for (permissions, expected, sync) in [
        (Permissions::Temporary, 0o600, false),
        (Permissions::Preserve, 0o640, true),
        (Permissions::Mode(0o644), 0o644, false),
    ] {
        fs::write(&source, "old").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&source, &path).unwrap();
        let mut old = fs::File::open(&path).unwrap();
        replace_atomically(&path, b"new", ReplaceOptions { permissions, sync }).unwrap();
        assert!(!path.is_symlink());
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            expected
        );
        let mut text = String::new();
        old.read_to_string(&mut text).unwrap();
        assert_eq!(text, "old");
        assert_eq!(fs::read_to_string(&source).unwrap(), "old");
        fs::remove_file(&path).unwrap();
    }
    replace_atomically(
        &path,
        b"created",
        ReplaceOptions {
            permissions: Permissions::Preserve,
            sync: false,
        },
    )
    .unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn failed_or_abandoned_replacements_leave_no_temporary_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("destination");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("keep"), "old").unwrap();
    assert!(
        replace_atomically(
            &path,
            b"new",
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: false,
            }
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(path.join("keep")).unwrap(), "old");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    let prepared = prepare_atomic_write(
        &path,
        b"new",
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: false,
        },
    )
    .unwrap();
    assert!(path.is_dir());
    drop(prepared);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn home_expansion_only_replaces_a_leading_tilde_component() {
    let home = Path::new("/home/test");
    for (input, expected) in [
        ("~", "/home/test"),
        ("~/dir", "/home/test/dir"),
        ("~/../dir", "/home/test/../dir"),
        ("~someone/dir", "~someone/dir"),
        ("dir/~", "dir/~"),
        ("/dir", "/dir"),
        ("", ""),
    ] {
        assert_eq!(expand_home(Path::new(input), home), Path::new(expected));
    }
    let path = Path::new(OsStr::from_bytes(b"~/non-utf8-\xff"));
    assert_eq!(
        expand_home(path, home),
        home.join(OsStr::from_bytes(b"non-utf8-\xff"))
    );
}

#[test]
fn executable_search_skips_invalid_candidates_and_preserves_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let program = second.join("tool");
    fs::write(&program, "executable").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o100)).unwrap();
    let search = std::env::join_paths([&first, &second]).unwrap();
    let candidate = first.join("tool");
    assert!(is_executable(&candidate).is_err());
    for directory in [false, true] {
        if directory {
            fs::remove_file(&candidate).unwrap();
            fs::create_dir(&candidate).unwrap();
        } else {
            fs::write(&candidate, "not executable").unwrap();
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(!is_executable(&candidate).unwrap());
        assert_eq!(
            find_executable(OsStr::new("tool"), &search),
            Some(program.clone())
        );
    }
    fs::remove_dir(&candidate).unwrap();
    symlink(&program, &candidate).unwrap();
    assert_eq!(
        find_executable(OsStr::new("tool"), &search),
        Some(candidate)
    );
    assert_eq!(find_executable(OsStr::new("missing"), &search), None);
}

#[test]
fn optional_read_distinguishes_missing_empty_and_invalid_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    assert_eq!(read_optional(&path).unwrap(), None);
    fs::write(&path, "").unwrap();
    assert_eq!(read_optional(&path).unwrap(), Some(String::new()));
    fs::write(&path, "text").unwrap();
    assert_eq!(read_optional(&path).unwrap().as_deref(), Some("text"));
    fs::write(&path, [0xff]).unwrap();
    assert_eq!(
        read_optional(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(read_optional(root.path()).is_err());
    let link = root.path().join("link");
    symlink(root.path().join("missing"), &link).unwrap();
    assert_eq!(read_optional(&link).unwrap(), None);
}
