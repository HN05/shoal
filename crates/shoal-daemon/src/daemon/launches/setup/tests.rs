use super::*;
use crate::daemon::{
    approvals::{Target, Undecided},
    test_support::{chat, manager, repository},
};
use shoal_protocol::launches::{LaunchSource, Setup};

/// The space of the chat `starter`, as a scoped caller that runs in it.
fn caller(starter: &crate::model::Chat) -> Caller {
    Caller {
        space_id: starter.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
    }
}

/// The approval request `error` asks for, which must be one.
fn requested(error: anyhow::Error) -> Box<shoal_protocol::approvals::ApprovalRequest> {
    match error.downcast::<Undecided>() {
        Ok(Undecided(request)) => request,
        Err(error) => panic!("expected an approval request: {error:#}"),
    }
}

/// A scoped caller starts a repository's setup only as its `admin`
/// permission allows, which a chat's mode leaves to an approval, asked
/// before anything is created.
#[tokio::test]
async fn a_scoped_repository_setup_asks_the_admin_permission() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let starter = chat(&manager, "starter", "chat", Some(&repo), None).await;
    let launch = Launch {
        repository: Some(repo.id.to_string()),
        branch: Some("shoal-setup".into()),
        setup: Some(Setup::Repository),
        ..Launch::default()
    };

    let error = (manager.launch(launch, LaunchSource::Manual, Some(&caller(&starter))))
        .await
        .unwrap_err();
    let request = requested(error);
    assert_eq!(request.target, Target::Action(PermissionKey::Admin));
    assert!(manager.list_workspaces().await.unwrap().is_empty());
}

/// A scoped caller starts the global config's setup only as its `admin`
/// permission allows without a repository, asked before its chat is
/// recorded.
#[tokio::test]
async fn a_scoped_config_setup_asks_the_admin_permission() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let starter = chat(&manager, "starter", "chat", Some(&repo), None).await;
    let launch = Launch {
        setup: Some(Setup::Config),
        prompt: Some("Set up the global config".into()),
        ..Launch::default()
    };

    let error = (manager.launch(launch, LaunchSource::Manual, Some(&caller(&starter))))
        .await
        .unwrap_err();
    let request = requested(error);
    assert_eq!(request.target, Target::Action(PermissionKey::Admin));
    assert_eq!(manager.chats().await.unwrap().len(), 1);
}
