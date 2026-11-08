use super::*;
use std::os::unix::fs::symlink;

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
