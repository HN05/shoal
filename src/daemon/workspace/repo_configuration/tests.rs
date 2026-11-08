use crate::client::space_names::SpaceSelector;
use crate::daemon::{notifications::NotificationKind, test_support};

/// A registered repository and its local config's directory, which does not
/// exist yet.
async fn repository_with_local_directory() -> (
    tempfile::TempDir,
    std::sync::Arc<super::Manager>,
    crate::model::Repository,
    std::path::PathBuf,
) {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let local = manager.local_config_path(&repo.id).await.unwrap();
    let directory = local.parent().unwrap().to_path_buf();
    let _ = std::fs::remove_dir(&directory);
    (root, manager, repo, directory)
}

#[tokio::test]
async fn a_workspace_recorded_where_the_local_config_belongs_is_never_local_config() {
    let (_root, manager, repo, directory) = repository_with_local_directory().await;
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    // An earlier version accepted this explicit workspace path.
    test_support::git(
        &repo.path,
        &[
            "worktree",
            "move",
            workspace.path.to_str().unwrap(),
            directory.to_str().unwrap(),
        ],
    );
    let (id, path) = (workspace.id.clone(), directory.clone());
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET path=?2 WHERE id=?1",
                rusqlite::params![id, path.to_str()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let tracked = directory.join("config.toml");
    std::fs::write(&tracked, "agent.default = 'codex'\n").unwrap();
    test_support::commit(&directory, "config.toml");
    let edit = manager.edit_local_config(&repo.id, Vec::new()).await;
    assert!(edit.is_err());
    assert_eq!(
        std::fs::read_to_string(&tracked).unwrap(),
        "agent.default = 'codex'\n"
    );
    // Settings resolve without it, so the workspace and repository go as usual.
    let settings = manager.repository_settings(&repo.id).await.unwrap();
    let agent = settings.agent.default.as_ref().map(ToString::to_string);
    assert_ne!(agent.as_deref(), Some("codex"));
    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert!(!directory.exists());
    manager.remove_repository(&repo.id).await.unwrap();
}

#[tokio::test]
async fn an_unrecorded_worktree_where_the_local_config_belongs_is_never_local_config() {
    let (_root, manager, repo, directory) = repository_with_local_directory().await;
    test_support::git(
        &repo.path,
        &[
            "worktree",
            "add",
            "-b",
            "other",
            directory.to_str().unwrap(),
        ],
    );
    let edit = manager.edit_local_config(&repo.id, Vec::new()).await;
    assert!(edit.is_err());
    assert!(!directory.join("config.toml").exists());
    manager.repository_settings(&repo.id).await.unwrap();
}

#[tokio::test]
async fn a_workspace_in_a_stalled_directory_refuses_its_layers_without_reading_them() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    // Reading this file fails with a parse error, so the stall error shows
    // the read never ran.
    std::fs::write(workspace.path.join(".shoal.toml"), "not toml [").unwrap();
    manager.file_access.stall_rechecking(&workspace.path);

    let target = crate::protocol::ConfigTarget::Workspace(SpaceSelector::id(&workspace.id));
    let error = format!("{:#}", manager.config_layers_for(target).await.unwrap_err());
    let directory = workspace.path.parent().unwrap().display().to_string();
    assert!(
        error.starts_with(&format!("file access in {directory} stalled")),
        "{error}"
    );
    assert!(
        error.contains("a macOS privacy prompt may be waiting for an answer"),
        "{error}"
    );
    let settings = manager.workspace_settings(&workspace).await.unwrap_err();
    assert!(format!("{settings:#}").contains("stalled"), "{settings:#}");

    // Once access recovers, the file is read and ignored as rejected, and
    // the layers say why.
    manager.file_access.recover_at(&workspace.path);
    let target = crate::protocol::ConfigTarget::Workspace(SpaceSelector::id(&workspace.id));
    let layers = manager.config_layers_for(target).await.unwrap();
    let rejected = layers.rejected_worktree_file.unwrap();
    assert!(rejected.contains("TOML parse error"), "{rejected}");
    assert_eq!(ignored_configs(&manager).await.len(), 1);
}

/// The unread notifications that a workspace's config file is ignored.
async fn ignored_configs(manager: &super::Manager) -> Vec<String> {
    (manager.notifications(true, 50).await.unwrap().into_iter())
        .filter(|notification| notification.kind == NotificationKind::ConfigIgnored)
        .map(|notification| notification.message)
        .collect()
}

async fn mark_all_read(manager: &super::Manager) {
    let unread = manager.notifications(true, 50).await.unwrap();
    let ids = unread
        .into_iter()
        .map(|notification| notification.id)
        .collect();
    manager.mark_notifications_read(ids).await.unwrap();
}

#[tokio::test]
async fn a_worktree_file_the_format_rejects_is_ignored_and_reported_once_per_error() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .write_local_config(&repo.id, Some("[cleanup.auto]\nidle_minutes = 3\n".into()))
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "old".into(), None, None, None)
        .await
        .unwrap();
    let file = workspace.path.join(".shoal.toml");
    std::fs::write(&file, "setup_cmd = \"scripts/setup.sh\"\n").unwrap();

    // The other layers still apply.
    let settings = manager.workspace_settings(&workspace).await.unwrap();
    assert_eq!(settings.cleanup.auto.idle_minutes, 3);
    let reported = ignored_configs(&manager).await;
    assert_eq!(reported.len(), 1);
    assert!(reported[0].contains("setup_cmd"), "{reported:?}");

    // Once read, the same error is not reported again.
    mark_all_read(&manager).await;
    manager.workspace_settings(&workspace).await.unwrap();
    assert!(ignored_configs(&manager).await.is_empty());

    // Another error is.
    std::fs::write(&file, "[setup]\ncmd = \"scripts/setup.sh\"\n").unwrap();
    manager.workspace_settings(&workspace).await.unwrap();
    assert_eq!(ignored_configs(&manager).await.len(), 1);
    mark_all_read(&manager).await;

    // A file the format accepts in between makes the next rejection new.
    std::fs::write(&file, "").unwrap();
    manager.workspace_settings(&workspace).await.unwrap();
    std::fs::write(&file, "[setup]\ncmd = \"scripts/setup.sh\"\n").unwrap();
    manager.workspace_settings(&workspace).await.unwrap();
    assert_eq!(ignored_configs(&manager).await.len(), 1);
}
