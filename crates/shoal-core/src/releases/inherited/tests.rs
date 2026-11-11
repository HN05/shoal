use std::fs;

use super::*;

#[test]
fn an_invoked_path_changes_once_an_upgrade_replaces_or_removes_its_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("shoal");
    fs::write(&path, "old").unwrap();
    let invoked = Invoked::at(path.clone());
    assert!(invoked.unchanged());

    fs::remove_file(&path).unwrap();
    assert!(
        !invoked.unchanged(),
        "a removed release is no longer invoked"
    );
    fs::write(&path, "new").unwrap();
    assert!(!invoked.unchanged(), "an upgrade put another binary there");
}

#[test]
fn an_invoked_path_missing_at_the_handover_is_never_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("shoal");
    let invoked = Invoked::at(path.clone());
    fs::write(&path, "installed later").unwrap();
    assert!(!invoked.unchanged());
}
