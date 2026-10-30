use crate::test_support;

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
    let error = format!("{:#}", edit.unwrap_err());
    assert!(error.contains("where the local config belongs"), "{error}");
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
            &workspace.id,
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
    let error = format!("{:#}", edit.unwrap_err());
    assert!(error.contains("where the local config belongs"), "{error}");
    assert!(!directory.join("config.toml").exists());
    manager.repository_settings(&repo.id).await.unwrap();
}
