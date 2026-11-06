use super::*;
use crate::{
    daemon::test_support::{manager, repository},
    state::WorkspaceState,
};

#[tokio::test]
async fn reverts_keep_the_reverted_base_and_skip_workspaces_being_removed() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let elsewhere = PullOptions {
        base: Some("release".into()),
        ..PullOptions::default()
    };
    let error = manager
        .revert_pull(&workspace.id, Some("7".into()), elsewhere, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("base branch"), "{error:#}");

    manager
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();
    // Refused before the forge is consulted: the repository has no origin.
    let error = manager
        .revert_pull(
            &workspace.id,
            Some("7".into()),
            PullOptions::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("cannot accept a PR"),
        "{error:#}"
    );
}

#[tokio::test]
async fn a_chat_reverts_from_its_repositorys_checkout() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    // The chat reaches its checkout's forge, which this repository lacks,
    // rather than being refused as no workspace.
    let reverted = manager
        .revert_pull(&chat.id, Some("7".into()), PullOptions::default(), None)
        .await;
    let merged = manager.merged_pull(&chat.name, Some("7".into())).await;
    for error in [reverted.unwrap_err(), merged.unwrap_err()] {
        assert!(
            format!("{error:#}").contains("need an origin remote"),
            "{error:#}"
        );
    }
}
