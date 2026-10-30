use std::os::unix::fs::PermissionsExt;

use super::*;

#[test]
fn directories_are_private_and_removed_one_at_a_time() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), "repo");
    let workspace = layout.prepare("workspace-id").unwrap();
    assert_eq!(workspace, root.path().join(".tmp/workspace-id"));
    let mode = fs::metadata(&workspace).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    fs::write(workspace.join("scratch"), "kept until removal").unwrap();
    assert_eq!(layout.prepare("workspace-id").unwrap(), workspace);
    assert!(
        workspace.join("scratch").exists(),
        "preparing again keeps files"
    );
    let chat = layout.prepare("chat-id").unwrap();

    layout.remove_one("workspace-id").unwrap();
    layout.remove_one("workspace-id").unwrap();
    assert!(!workspace.exists());
    assert!(chat.exists());
    layout.remove().unwrap();
    assert!(!layout.root().exists());
}

#[test]
fn only_a_directory_marked_for_the_repository_is_used() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), "repo");
    fs::create_dir_all(layout.root().join("user-files")).unwrap();
    let error = layout.prepare("workspace-id").unwrap_err();
    assert!(
        error.to_string().contains("not a directory Shoal created"),
        "{error:#}"
    );
    layout.remove_one("user-files").unwrap();
    layout.remove().unwrap();
    assert!(
        layout.root().join("user-files").exists(),
        "the user's stays"
    );
}

#[test]
fn a_symlink_in_place_of_a_directory_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), "repo");
    layout.prepare("other").unwrap();
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, layout.locate("linked")).unwrap();
    assert!(layout.prepare("linked").is_err());
}
