use shoal_core::test_support::{global_config, repository};
use shoal_protocol::{model::Repository, protocol::ItemContext, space_names::SpaceSelector};

use crate::stand_in::{StandIn, record, registered, service};

#[tokio::test]
async fn a_space_naming_its_own_repository_acts_there_as_itself() {
    let root = tempfile::tempdir().unwrap();
    let store = shoal_store::test_support::unsynced_store(root.path().join("state.db"))
        .await
        .unwrap();
    let path = repository(root.path(), "repo");
    let repos = [
        registered("repo", &path),
        registered("other", &repository(root.path(), "other")),
    ];
    let workspace = shoal_protocol::test_support::workspace("workspace", "repo", "topic", &path);
    record(&store, &workspace).await;
    let links = service(root.path(), store, "");
    let daemon = StandIn {
        workspaces: vec![workspace.clone()],
        repositories: repos.to_vec(),
        config: Some(global_config("")),
        ..StandIn::default()
    };
    let context = |repository: &Repository| ItemContext::Space {
        space: SpaceSelector::id(&workspace.id),
        repository: Some(repository.id.clone()),
    };

    let own = links.context_space(&daemon, &context(&repos[0])).await;
    let own = own.unwrap();
    assert_eq!(
        (
            own.id.clone(),
            own.repository_id().clone(),
            own.keeps_links()
        ),
        (workspace.id.clone(), repos[0].id.clone(), true)
    );
    let other = links.context_space(&daemon, &context(&repos[1])).await;
    assert!(other.is_err());
}
