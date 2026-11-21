use super::*;
use crate::test_support::write_beside_global_config;

fn read(path: &Path) -> Option<String> {
    crate::fsutil::read_optional(path).unwrap()
}

#[test]
fn a_copy_keeps_the_file_it_replaces_only_when_asked() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let mode = FileName::Mode("review".into());
    let path = Shared::file(&paths, &mode);
    let backup = path.with_file_name("review.md.backup");
    let held = hold(&paths).unwrap();
    assert_eq!(
        held.write(&paths, &mode, Some("first"), true).unwrap(),
        None
    );
    assert_eq!(
        held.write(&paths, &mode, Some("second"), false).unwrap(),
        None
    );
    assert_eq!((read(&path), read(&backup)), (Some("second".into()), None));
    let kept = held.write(&paths, &mode, Some("third"), true).unwrap();
    assert_eq!(kept, Some(backup.clone()));
    assert_eq!(read(&backup).as_deref(), Some("second"));
    assert_eq!(read(&path).as_deref(), Some("third"));
}

#[test]
fn a_removal_keeps_the_file_it_removes_only_when_asked() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    write_beside_global_config(&paths, "shared.toml", "edited\n");
    let path = Shared::path(&paths);
    let backup = path.with_file_name("shared.toml.backup");
    let held = hold(&paths).unwrap();
    let kept = held.write(&paths, &FileName::Config, None, true).unwrap();
    assert_eq!(kept, Some(backup.clone()));
    assert_eq!(
        (read(&path), read(&backup)),
        (None, Some("edited\n".into()))
    );
    assert_eq!(
        held.write(&paths, &FileName::Config, None, true).unwrap(),
        None
    );
    write_beside_global_config(&paths, "shared.toml", "again\n");
    assert_eq!(
        held.write(&paths, &FileName::Config, None, false).unwrap(),
        None
    );
    assert_eq!(
        (read(&path), read(&backup)),
        (None, Some("edited\n".into()))
    );
}
