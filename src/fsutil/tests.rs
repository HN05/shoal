use super::*;
use std::{fs, io, os::unix::fs::symlink};

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
