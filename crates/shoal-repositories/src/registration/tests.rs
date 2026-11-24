use shoal_core::test_support::repository;

use crate::{
    root::tests::excluded_root, root::tests::nothing, spotlight::Layout, stand_in::Fixture,
};

#[tokio::test]
async fn a_contribution_clones_a_url_and_keeps_an_existing_registration() {
    let fixture = Fixture::new().await;
    let repositories = &fixture.repositories;
    let path = repository(fixture.root.path(), "repo");
    let source = path.to_str().unwrap().to_owned();
    let contributed = repositories.register_contribution(source.clone(), None);
    assert!(contributed.await.is_err());
    let registered = repositories
        .register(source.as_str().into(), None, None)
        .await
        .unwrap();
    let again = repositories.register_contribution(source, None);
    let (again, _hold) = again.await.unwrap();
    assert_eq!(again.id, registered.id);
    assert!(!again.contribution);
}

#[tokio::test]
async fn registration_keeps_a_checkout_in_a_root_spotlight_indexes() {
    let (_temp, parent, mut root) = excluded_root();
    root.layout = Layout::AsFound;
    let project = root.path.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let checkout = repository(&project, "checkout");
    let (path, directory) = super::local_checkout_location(
        checkout.to_str().unwrap(),
        &root,
        &parent.join("state"),
        "checkout",
        &nothing(),
    )
    .await
    .unwrap();
    assert_eq!(path, checkout);
    assert_eq!(directory, project);
}

#[tokio::test]
async fn no_clone_lands_where_its_own_local_config_belongs() {
    let fixture = Fixture::new().await;
    let root = fixture.root_dir();
    let cloned = fixture
        .repositories
        .register(
            "https://example.test/team/project.git".into(),
            Some("project".into()),
            Some(root.join("project").join(".shoal")),
        )
        .await;
    assert!(cloned.is_err());
    assert!(!root.join("project").exists());
}
