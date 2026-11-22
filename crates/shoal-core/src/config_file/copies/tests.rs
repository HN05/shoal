use super::*;

fn read(path: &Path) -> Option<String> {
    crate::fsutil::read_optional(path).unwrap()
}

#[test]
fn a_copy_keeps_the_file_it_replaces_only_when_asked() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let backup = directory.path().join("config.toml.backup");
    let held = hold(&path).unwrap();
    assert_eq!(held.write(&path, Some("first"), true).unwrap(), None);
    assert_eq!(held.write(&path, Some("second"), false).unwrap(), None);
    assert_eq!((read(&path), read(&backup)), (Some("second".into()), None));
    let kept = held.write(&path, Some("third"), true).unwrap();
    assert_eq!(kept, Some(backup.clone()));
    assert_eq!(read(&backup).as_deref(), Some("second"));
    assert_eq!(read(&path).as_deref(), Some("third"));
}

#[test]
fn a_removal_keeps_the_file_it_removes_only_when_asked() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared.toml");
    let backup = directory.path().join("shared.toml.backup");
    fs::write(&path, "edited\n").unwrap();
    let held = hold(&path).unwrap();
    let kept = held.write(&path, None, true).unwrap();
    assert_eq!(kept, Some(backup.clone()));
    assert_eq!(
        (read(&path), read(&backup)),
        (None, Some("edited\n".into()))
    );
    assert_eq!(held.write(&path, None, true).unwrap(), None);
    fs::write(&path, "again\n").unwrap();
    assert_eq!(held.write(&path, None, false).unwrap(), None);
    assert_eq!(
        (read(&path), read(&backup)),
        (None, Some("edited\n".into()))
    );
}
