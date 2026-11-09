use crate::client::space_ids::SpaceId;
use std::os::unix::fs::PermissionsExt;

use super::*;

#[test]
fn directories_are_private_listed_and_removed_one_at_a_time() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), &"repo".into());
    let workspace = layout.prepare(&SpaceId::from("workspace-id")).unwrap();
    assert_eq!(workspace, root.path().join(".tmp/workspace-id"));
    let mode = fs::metadata(&workspace).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    fs::write(workspace.join("scratch"), "kept until removal").unwrap();
    assert_eq!(
        layout.prepare(&SpaceId::from("workspace-id")).unwrap(),
        workspace
    );
    assert!(
        workspace.join("scratch").exists(),
        "preparing again keeps files"
    );
    let chat = layout.prepare(&SpaceId::from("chat-id")).unwrap();
    fs::write(layout.root().join("stray-file"), "").unwrap();

    assert_eq!(layout.list().unwrap(), ["chat-id", "workspace-id"]);
    layout.remove_one(&SpaceId::from("workspace-id")).unwrap();
    layout.remove_one(&SpaceId::from("workspace-id")).unwrap();
    assert_eq!(layout.list().unwrap(), ["chat-id"]);
    assert!(chat.exists());
    layout.remove().unwrap();
    assert!(!layout.root().exists());
}

#[test]
fn only_a_directory_marked_for_the_repository_is_used() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), &"repo".into());
    fs::create_dir_all(layout.root().join("user-files")).unwrap();
    assert!(layout.prepare(&SpaceId::from("workspace-id")).is_err());
    assert!(layout.list().unwrap().is_empty());
    layout.remove_one(&SpaceId::from("user-files")).unwrap();
    layout.remove().unwrap();
    assert!(
        layout.root().join("user-files").exists(),
        "the user's stays"
    );
}

#[test]
fn a_symlink_in_place_of_a_directory_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), &"repo".into());
    layout.prepare(&SpaceId::from("other")).unwrap();
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, layout.locate(&SpaceId::from("linked"))).unwrap();
    assert!(layout.prepare(&SpaceId::from("linked")).is_err());
}

/// What works in a space's temporary directory counts as the space's, so
/// only the directory Shoal created leads there, never a link put in its
/// place.
#[test]
fn only_a_created_directory_is_an_existing_one() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), &"repo".into());
    assert_eq!(layout.existing(&SpaceId::from("space")), None);
    let created = layout.prepare(&SpaceId::from("space")).unwrap();
    assert_eq!(layout.existing(&SpaceId::from("space")), Some(created));

    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, layout.locate(&SpaceId::from("linked"))).unwrap();
    assert_eq!(layout.existing(&SpaceId::from("linked")), None);

    let unmarked = tempfile::tempdir().unwrap();
    fs::create_dir_all(unmarked.path().join(".tmp/space")).unwrap();
    assert_eq!(
        Layout::new(unmarked.path(), &"repo".into()).existing(&SpaceId::from("space")),
        None
    );
}

#[test]
fn a_directory_another_daemon_marked_stays_its_own() {
    let root = tempfile::tempdir().unwrap();
    let first = Layout::outside_repositories(root.path(), Path::new("/first/state"));
    first.prepare(&SpaceId::from("master")).unwrap();
    let second = Layout::outside_repositories(root.path(), Path::new("/second/state"));
    let error = second
        .prepare(&SpaceId::from("other"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("/first/state"), "{error}");
    assert!(!error.contains("move it away"), "{error}");
    assert!(root.path().join(".tmp/master").is_dir());
}
