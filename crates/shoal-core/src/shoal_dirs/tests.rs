use std::fs;

use super::*;

#[test]
fn a_directory_another_daemon_marked_stays_its_own() {
    let root = tempfile::tempdir().unwrap();
    let first =
        ShoalDir::outside_repositories(root.path(), Path::new("/first/state"), Contents::TmpDirs);
    first.create().unwrap();
    let second =
        ShoalDir::outside_repositories(root.path(), Path::new("/second/state"), Contents::TmpDirs);
    let error = second.create().unwrap_err().to_string();
    assert!(error.contains("/first/state"), "{error}");
    assert!(!error.contains("move it away"), "{error}");
    assert!(first.is_own());
    second.remove().unwrap();
    assert!(first.root().is_dir());
}

#[test]
fn a_directory_shoal_did_not_create_is_refused_and_kept() {
    let root = tempfile::tempdir().unwrap();
    let dir = ShoalDir::new(root.path(), &"repo".into(), Contents::Attachments);
    fs::create_dir_all(dir.root().join("user-files")).unwrap();
    let error = dir.create().unwrap_err().to_string();
    assert!(error.contains("move it away"), "{error}");
    dir.remove().unwrap();
    assert!(dir.root().join("user-files").exists(), "the user's stays");
}

#[test]
fn a_symlink_in_place_of_the_directory_is_not_its_own() {
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside");
    let dir = ShoalDir::new(outside.as_path(), &"repo".into(), Contents::TmpDirs);
    dir.create().unwrap();
    let linked = ShoalDir::new(root.path(), &"repo".into(), Contents::TmpDirs);
    std::os::unix::fs::symlink(dir.root(), linked.root()).unwrap();
    assert!(!linked.is_own());
    linked.remove().unwrap();
    assert!(dir.root().is_dir());
}
