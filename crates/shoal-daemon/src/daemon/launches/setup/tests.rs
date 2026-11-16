use super::*;
use crate::daemon::{
    approvals::{Target, Undecided},
    test_support::{chat, manager, repository},
};
use shoal_protocol::launches::LaunchSource;

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
    let caller = Caller {
        space_id: starter.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
    };
    let launch = Launch {
        repository: Some(repo.id.to_string()),
        branch: Some("shoal-setup".into()),
        repository_setup: true,
        ..Launch::default()
    };

    let error = (manager.launch(launch, LaunchSource::Manual, Some(&caller)))
        .await
        .unwrap_err();
    let request = match error.downcast::<Undecided>() {
        Ok(Undecided(request)) => request,
        Err(error) => panic!("expected an approval request: {error:#}"),
    };
    assert_eq!(request.target, Target::Action(PermissionKey::Admin));
    assert!(manager.list_workspaces().await.unwrap().is_empty());
}
