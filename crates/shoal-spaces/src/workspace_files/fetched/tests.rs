use crate::stand_in::Fixture;
use shoal_core::test_support::repository;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn fetched_copies_go_beside_the_workspaces_of_their_repository() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "charts".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let directory = manager
        .spaces
        .fetched_directory(&*manager, None, &selector)
        .await
        .unwrap();
    let home = crate::shoal_dirs::ShoalDir::of(&repo, crate::shoal_dirs::Contents::Fetched);
    assert_eq!(directory.path, home.root().join(workspace.id.as_str()));
    assert!(home.is_own() && directory.path.is_dir());
}
