use crate::conversions::to_workspace::WorkspaceConversion;
use crate::stand_in::Fixture;
use crate::{config::modes::ModeName, model::Repository, scope::Caller};
use shoal_core::test_support::repository;
use shoal_protocol::space_names::SpaceSelector;

async fn registered(root: &std::path::Path, manager: &Fixture) -> Repository {
    let path = repository(root, "repo");
    manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

fn conversion() -> WorkspaceConversion {
    WorkspaceConversion {
        branch: "fix-login".into(),
        issue: None,
        base: None,
        options: Default::default(),
        mode: ModeName::DEFAULT,
        agent: Default::default(),
    }
}

/// The refusal of converting the space `selector` names for `caller`.
async fn refusal(manager: &Fixture, selector: SpaceSelector, caller: Option<&Caller>) -> String {
    let refused = manager
        .spaces
        .convert_to_workspace(&manager.daemon(), &selector, conversion(), None, caller)
        .await
        .unwrap_err();
    format!("{refused:#}")
}

#[tokio::test]
async fn a_workspace_is_a_workspace_already() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();

    let refused = refusal(&manager, SpaceSelector::id(&workspace.id), None).await;

    assert!(refused.contains("is a workspace already"), "{refused}");
}
