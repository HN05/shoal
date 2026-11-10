use super::*;

#[test]
fn a_chat_is_refused_a_workspace_that_is_not_ready_before_anything_is_asked() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::new_record(
        "repo".into(),
        "worker".into(),
        root.path().to_owned(),
        Some("worker".into()),
        crate::state::WorkspaceState::Ready,
    );
    ensure_hosts_chat(&workspace).unwrap();
    workspace.state = crate::state::WorkspaceState::Failed;
    assert!(ensure_hosts_chat(&workspace).is_err());
    workspace.state = crate::state::WorkspaceState::Ready;
    workspace.path = root.path().join("gone");
    assert!(ensure_hosts_chat(&workspace).is_err());
}
