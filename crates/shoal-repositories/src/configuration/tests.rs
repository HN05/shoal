use shoal_core::test_support::{git, repository};
use shoal_protocol::{alerts::AlertKind, protocol::ConfigTarget, space_names::SpaceSelector};

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

#[tokio::test]
async fn a_workspace_in_a_stalled_directory_refuses_its_layers_without_reading_them() {
    let fixture = Fixture::new().await;
    let repo = (fixture.register(repository(fixture.root.path(), "repo").display())).await;
    let workspace = fixture.record_workspace(&repo, "worker", None).await;
    // Reading this file fails with a parse error, so the stall error shows
    // the read never ran.
    std::fs::write(workspace.path.join(".shoal.toml"), "not toml [").unwrap();
    fixture.file_access.stall_rechecking(&workspace.path);
    let (repositories, daemon) = (&fixture.repositories, &fixture.daemon);
    let target = || ConfigTarget::Space(SpaceSelector::id(&workspace.id));

    let error = (repositories.config_layers_for(daemon, target()).await).unwrap_err();
    let directory = workspace.path.parent().unwrap().display().to_string();
    let error = format!("{error:#}");
    assert!(
        error.starts_with(&format!("file access in {directory} timed out")),
        "{error}"
    );
    let settings = repositories.workspace_settings(daemon, &workspace).await;
    let settings = format!("{:#}", settings.unwrap_err());
    assert!(settings.contains("timed out"), "{settings}");

    // Once access recovers, the file is read and ignored as rejected, and
    // the layers say why.
    fixture.file_access.recover_at(&workspace.path);
    let layers = repositories.config_layers_for(daemon, target()).await;
    let rejected = layers.unwrap().rejected_worktree_file.unwrap();
    assert!(rejected.contains("TOML parse error"), "{rejected}");
    assert_eq!(ignored_configs(&fixture).await.len(), 1);
}

/// The unread notifications that a workspace's config file is ignored.
async fn ignored_configs(fixture: &Fixture) -> Vec<String> {
    let unread = fixture.repositories.notifications.list(true, 50).await;
    (unread.unwrap().into_iter())
        .filter(|notification| notification.kind == AlertKind::ConfigIgnored)
        .map(|notification| notification.message)
        .collect()
}

async fn mark_all_read(fixture: &Fixture) {
    let notifications = &fixture.repositories.notifications;
    let unread = notifications.list(true, 50).await.unwrap();
    let ids = (unread.into_iter())
        .map(|notification| notification.id)
        .collect();
    notifications.mark_read(ids).await.unwrap();
}

#[tokio::test]
async fn a_worktree_file_the_format_rejects_is_ignored_and_reported_once_per_error() {
    let fixture = Fixture::new().await;
    let repo = (fixture.register(repository(fixture.root.path(), "repo").display())).await;
    let local = "[cleanup.auto]\nidle_minutes = 3\n";
    fixture.write_local_config(&repo.id, local).await;
    let workspace = fixture.record_workspace(&repo, "old", None).await;
    let file = workspace.path.join(".shoal.toml");
    std::fs::write(&file, "setup_cmd = \"scripts/setup.sh\"\n").unwrap();
    let settings = || (fixture.repositories).workspace_settings(&fixture.daemon, &workspace);

    // The other layers still apply.
    assert_eq!(settings().await.unwrap().cleanup.auto.idle_minutes, 3);
    let reported = ignored_configs(&fixture).await;
    assert_eq!(reported.len(), 1);
    assert!(reported[0].contains("setup_cmd"), "{reported:?}");

    // Once read, the same error is not reported again.
    mark_all_read(&fixture).await;
    settings().await.unwrap();
    assert!(ignored_configs(&fixture).await.is_empty());

    // Another error is.
    std::fs::write(&file, "[setup]\ncmd = \"scripts/setup.sh\"\n").unwrap();
    settings().await.unwrap();
    assert_eq!(ignored_configs(&fixture).await.len(), 1);
    mark_all_read(&fixture).await;

    // A file the format accepts in between makes the next rejection new.
    std::fs::write(&file, "").unwrap();
    settings().await.unwrap();
    std::fs::write(&file, "[setup]\ncmd = \"scripts/setup.sh\"\n").unwrap();
    settings().await.unwrap();
    assert_eq!(ignored_configs(&fixture).await.len(), 1);
}

/// A chat's config is its repository's, read from the registered checkout,
/// even for a chat in a workspace's worktree, whose file is the workspace's.
#[tokio::test]
async fn a_chats_layers_are_its_repositorys() {
    let fixture = Fixture::new().await;
    let checkout = repository(fixture.root.path(), "repo");
    let repo = fixture.register(checkout.display()).await;
    let workspace = fixture.record_workspace(&repo, "worker", None).await;
    std::fs::write(checkout.join(".shoal.toml"), "git_profile = 'checkout'\n").unwrap();
    let worktree_file = "git_profile = 'worktree'\n";
    std::fs::write(workspace.path.join(".shoal.toml"), worktree_file).unwrap();
    let chat = fixture.record_chat_in(&workspace);

    let read = |id: &shoal_ids::space_ids::SpaceId| {
        let target = ConfigTarget::Space(SpaceSelector::id(id));
        (fixture.repositories).config_layers_for(&fixture.daemon, target)
    };
    let chat = read(&chat.id).await.unwrap();
    let workspace = read(&workspace.id).await.unwrap();
    assert_eq!(chat.worktree_file.git_profile.as_deref(), Some("checkout"));
    assert_eq!(
        workspace.worktree_file.git_profile.as_deref(),
        Some("worktree")
    );
}
