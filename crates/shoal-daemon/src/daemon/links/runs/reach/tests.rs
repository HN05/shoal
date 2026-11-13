use crate::daemon::{
    scope::Caller,
    test_support::{chat, manager, repository, workspace_below},
};

/// A scope reaches the runs of the default branch and of the branches of
/// the workspaces it reaches; a master session's reaches every workspace's.
#[tokio::test]
async fn a_scope_reaches_the_default_branch_and_its_workspaces_branches() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let worker = create("worker").await.unwrap();
    let other = create("other").await.unwrap();
    let child = workspace_below(&manager, &repo, "child", &worker.id, None).await;
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let branch = |workspace: &crate::model::Workspace| workspace.branch.clone().unwrap();
    let (child, other) = (branch(&child), branch(&other));
    let reaches = async |space: &crate::client::space_ids::SpaceId, branch: &str| {
        let caller = Caller {
            space_id: space.clone(),
            execution: None,
        };
        manager
            .branch_in_reach(&caller, &repo.id, branch)
            .await
            .unwrap()
    };

    assert!(reaches(&worker.id, child.as_str()).await);
    assert!(reaches(&worker.id, "main").await);
    assert!(!reaches(&worker.id, other.as_str()).await);
    assert!(reaches(&master.id, other.as_str()).await);
}
