use crate::model::Space;
use crate::stand_in::Fixture;

use shoal_protocol::{model::Head, model::Workspace, state::WorkspaceState};

/// A ready workspace record on `branch`, which needs no worktree.
fn on_branch(workspace: &Workspace, branch: &str) -> Space {
    let mut workspace = workspace.clone();
    workspace.part.head = Head::Branch(branch.into());
    Space::from(workspace)
}

#[tokio::test]
async fn a_renamed_branch_changes_what_a_package_holds() {
    let manager = Fixture::new().await;
    let workspace = Workspace::new_record(
        "repo".into(),
        "work".into(),
        "/work".into(),
        Head::Branch("one".into()),
        WorkspaceState::Ready,
    );
    let before = manager
        .spaces
        .record_state(&*manager, &on_branch(&workspace, "one"))
        .await;
    let after = manager
        .spaces
        .record_state(&*manager, &on_branch(&workspace, "two"))
        .await;
    assert_ne!(before.unwrap(), after.unwrap());
}
