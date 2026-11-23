use crate::stand_in::Fixture;
use shoal_core::test_support::repository;

use crate::model::Workspace;
use shoal_protocol::space_names::SpaceSelector;

/// A workspace `root`, `middle` below it and `leaf` below `middle`, built on
/// its branch.
async fn tree(manager: &Fixture, root_dir: &std::path::Path) -> [Workspace; 3] {
    let checkout = repository(root_dir, "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let root = manager
        .create_workspace(&repo.id, "root".into(), None, None, None)
        .await
        .unwrap();
    let middle = child(manager, &root, "middle", None).await;
    let leaf = child(manager, &middle, "leaf", Some("middle")).await;
    [root, middle, leaf]
}

/// A new workspace `name` below `parent`, built on `base`.
async fn child(manager: &Fixture, parent: &Workspace, name: &str, base: Option<&str>) -> Workspace {
    let parent = SpaceSelector::id(&parent.id);
    let base = base.map(str::to_owned);
    (manager
        .spaces
        .create_child(&**manager, &parent, name.into(), base, Default::default()))
    .await
    .unwrap()
}

#[tokio::test]
async fn a_child_of_a_top_level_space_moves_to_the_top_level() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let [_, middle, _] = tree(&manager, temp.path()).await;

    let above = manager
        .spaces
        .move_up_one_level(&*manager, &middle.id, None)
        .await
        .unwrap();

    assert_eq!(above, None);
    let middle = manager.spaces.workspace_by_id(&middle.id).await.unwrap();
    assert_eq!(middle.parent, None);
    let top = manager
        .spaces
        .move_up_one_level(&*manager, &middle.id, None)
        .await;
    assert!(top.is_err(), "a top-level space has no level above it");
}
