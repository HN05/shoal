use shoal_core::test_support::{git, repository};

use crate::stand_in::Fixture;

#[tokio::test]
async fn an_unrecorded_worktree_where_the_local_config_belongs_is_never_local_config() {
    let fixture = Fixture::new().await;
    let repositories = &fixture.repositories;
    let checkout = repository(fixture.root.path(), "repo");
    let repo = repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let local = repositories.local_config_path(&repo.id).await.unwrap();
    let directory = local.parent().unwrap().to_path_buf();
    let _ = std::fs::remove_dir(&directory);
    let added = [
        "worktree",
        "add",
        "-b",
        "other",
        directory.to_str().unwrap(),
    ];
    git(&repo.path, &added);
    let edit = repositories.edit_local_config(&fixture.daemon, &repo.id, Vec::new());
    assert!(edit.await.is_err());
    assert!(!directory.join("config.toml").exists());
    repositories.repository_settings(&repo.id).await.unwrap();
}

#[tokio::test]
async fn a_checkout_where_the_local_config_belongs_gets_its_own_directory() {
    let fixture = Fixture::new().await;
    let repositories = &fixture.repositories;
    let root = fixture.root_dir();
    std::fs::create_dir_all(root.join("project")).unwrap();
    let checkout = repository(&root.join("project"), ".shoal");
    let repo = repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let local = repositories.local_config_path(&repo.id).await.unwrap();
    let checkout = std::fs::canonicalize(&checkout).unwrap();
    assert!(!local.starts_with(&checkout), "{}", local.display());
}

#[tokio::test]
async fn no_checkout_registers_where_another_repositorys_local_config_belongs() {
    let fixture = Fixture::new().await;
    let repositories = &fixture.repositories;
    let checkout = repository(fixture.root.path(), "project");
    let project = repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let local = repositories.local_config_path(&project.id).await.unwrap();
    let directory = local.parent().unwrap();
    std::fs::create_dir_all(directory).unwrap();
    let other = repository(directory, "other");
    let registered = repositories
        .register(other.to_str().unwrap().into(), None, None)
        .await;
    assert!(registered.is_err());
    let cloned = repositories
        .register(
            "https://example.test/team/other.git".into(),
            None,
            Some(directory.join("clone")),
        )
        .await;
    assert!(cloned.is_err());
    assert!(!directory.join("clone").exists());
}
