use crate::{
    model::{Swarm, WorkspaceRef},
    removal::{BranchChoice, InspectionPolicy},
    test_support::{manager, repository},
};

fn reference(workspace: &crate::model::Workspace) -> WorkspaceRef {
    WorkspaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        branch: workspace.branch.clone(),
    }
}

#[tokio::test]
async fn swarm_workspaces_share_their_task_until_one_remains() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let attempt = |name: &str, task: &str| {
        let manager = manager.clone();
        let (repo, name, task) = (repo.id.clone(), name.to_owned(), task.to_owned());
        async move {
            manager
                .create_swarm_workspace(&repo, name, None, None, None, task)
                .await
                .unwrap()
        }
    };
    let codex = attempt("fix-codex", "fix").await;
    let claude = attempt("fix-claude", "fix").await;
    let again = attempt("fix-claude-2", "fix").await;
    let other = attempt("other-codex", "other").await;
    let plain = manager
        .create_workspace(&repo.id, "plain".into(), None, None, None)
        .await
        .unwrap();
    assert_eq!(
        manager.workspace(&codex.id).await.unwrap().swarm,
        Some(Swarm {
            task: "fix".into(),
            workspaces: vec![reference(&claude), reference(&again)],
        })
    );
    assert_eq!(
        manager.workspace(&other.id).await.unwrap().swarm,
        Some(Swarm {
            task: "other".into(),
            workspaces: Vec::new(),
        })
    );
    assert_eq!(plain.swarm, None);
    assert!(
        manager
            .create_swarm_workspace(&repo.id, "blank".into(), None, None, None, " ".into())
            .await
            .is_err()
    );

    let remove = |id: String| {
        let manager = manager.clone();
        async move {
            manager
                .remove_workspace(&id, BranchChoice::Auto, InspectionPolicy::GitOnly)
                .await
                .unwrap()
        }
    };
    remove(again.id).await;
    assert_eq!(
        manager.workspace(&codex.id).await.unwrap().swarm,
        Some(Swarm {
            task: "fix".into(),
            workspaces: vec![reference(&claude)],
        })
    );
    remove(claude.id).await;
    assert_eq!(manager.workspace(&codex.id).await.unwrap().swarm, None);
}
