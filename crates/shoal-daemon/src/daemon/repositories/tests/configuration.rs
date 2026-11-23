//! The configuration layers of the workspaces and chats of a repository.
use crate::daemon::{notifications::AlertKind, test_support};
use shoal_protocol::space_names::SpaceSelector;

/// A registered repository and its local config's directory, which does not
/// exist yet.
async fn repository_with_local_directory() -> (
    tempfile::TempDir,
    std::sync::Arc<crate::daemon::manager::Manager>,
    crate::model::Repository,
    std::path::PathBuf,
) {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let local = manager
        .repositories
        .local_config_path(&repo.id)
        .await
        .unwrap();
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
    let edit = manager
        .repositories
        .edit_local_config(&*manager, &repo.id, Vec::new())
        .await;
    assert!(edit.is_err());
    assert_eq!(
        std::fs::read_to_string(&tracked).unwrap(),
        "agent.default = 'codex'\n"
    );
    // Settings resolve without it, so the workspace and repository go as usual.
    let settings = manager
        .repositories
        .repository_settings(&repo.id)
        .await
        .unwrap();
    let agent = settings.agent.default.as_ref().map(ToString::to_string);
    assert_ne!(agent.as_deref(), Some("codex"));
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert!(!directory.exists());
    manager
        .repositories
        .remove(&*manager, &repo.id)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_workspace_in_a_stalled_directory_refuses_its_layers_without_reading_them() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
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

    let target = crate::protocol::ConfigTarget::Space(SpaceSelector::id(&workspace.id));
    let error = format!(
        "{:#}",
        manager
            .repositories
            .config_layers_for(&*manager, target)
            .await
            .unwrap_err()
    );
    let directory = workspace.path.parent().unwrap().display().to_string();
    assert!(
        error.starts_with(&format!("file access in {directory} timed out")),
        "{error}"
    );
    let settings = manager
        .repositories
        .workspace_settings(&*manager, &workspace)
        .await
        .unwrap_err();
    assert!(
        format!("{settings:#}").contains("timed out"),
        "{settings:#}"
    );

    // Once access recovers, the file is read and ignored as rejected, and
    // the layers say why.
    manager.file_access.recover_at(&workspace.path);
    let target = crate::protocol::ConfigTarget::Space(SpaceSelector::id(&workspace.id));
    let layers = manager
        .repositories
        .config_layers_for(&*manager, target)
        .await
        .unwrap();
    let rejected = layers.rejected_worktree_file.unwrap();
    assert!(rejected.contains("TOML parse error"), "{rejected}");
    assert_eq!(ignored_configs(&manager).await.len(), 1);
}

/// The unread notifications that a workspace's config file is ignored.
async fn ignored_configs(manager: &crate::daemon::manager::Manager) -> Vec<String> {
    (manager
        .notifications
        .list(true, 50)
        .await
        .unwrap()
        .into_iter())
    .filter(|notification| notification.kind == AlertKind::ConfigIgnored)
    .map(|notification| notification.message)
    .collect()
}

async fn mark_all_read(manager: &crate::daemon::manager::Manager) {
    let unread = manager.notifications.list(true, 50).await.unwrap();
    let ids = unread
        .into_iter()
        .map(|notification| notification.id)
        .collect();
    manager.notifications.mark_read(ids).await.unwrap();
}

#[tokio::test]
async fn a_worktree_file_the_format_rejects_is_ignored_and_reported_once_per_error() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
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
    let settings = manager
        .repositories
        .workspace_settings(&*manager, &workspace)
        .await
        .unwrap();
    assert_eq!(settings.cleanup.auto.idle_minutes, 3);
    let reported = ignored_configs(&manager).await;
    assert_eq!(reported.len(), 1);
    assert!(reported[0].contains("setup_cmd"), "{reported:?}");

    // Once read, the same error is not reported again.
    mark_all_read(&manager).await;
    manager
        .repositories
        .workspace_settings(&*manager, &workspace)
        .await
        .unwrap();
    assert!(ignored_configs(&manager).await.is_empty());

    // Another error is.
    std::fs::write(&file, "[setup]\ncmd = \"scripts/setup.sh\"\n").unwrap();
    manager
        .repositories
        .workspace_settings(&*manager, &workspace)
        .await
        .unwrap();
    assert_eq!(ignored_configs(&manager).await.len(), 1);
    mark_all_read(&manager).await;

    // A file the format accepts in between makes the next rejection new.
    std::fs::write(&file, "").unwrap();
    manager
        .repositories
        .workspace_settings(&*manager, &workspace)
        .await
        .unwrap();
    std::fs::write(&file, "[setup]\ncmd = \"scripts/setup.sh\"\n").unwrap();
    manager
        .repositories
        .workspace_settings(&*manager, &workspace)
        .await
        .unwrap();
    assert_eq!(ignored_configs(&manager).await.len(), 1);
}

#[tokio::test]
async fn a_lease_is_kept_while_the_file_that_may_name_its_hooks_is_rejected() {
    use crate::{daemon::resources::ResourceRequest, hooks::HookKind};
    let (root, manager) = test_support::manager().await;
    test_support::configure(&manager, "[resources.lock]\n").await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "old".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let request = ResourceRequest {
        mode: None,
        kind: None,
        pool: "lock".into(),
        name: "held".into(),
        resource: None,
        reason: None,
        queue: false,
    };
    manager
        .leases
        .acquire_resource(&*manager, &selector, request, None)
        .await
        .unwrap();
    std::fs::write(workspace.path.join(".shoal.toml"), "setup_cmd = \"x\"\n").unwrap();

    // Other hooks go with the ignored file.
    let pre_remove = manager
        .spaces
        .workspace_hook(&*manager, &workspace, HookKind::PreRemove);
    assert_eq!(pre_remove.await.unwrap(), None);
    let release =
        manager
            .leases
            .release_resource(&*manager, &selector, "lock".into(), "held".into());
    let error = format!("{:#}", release.await.unwrap_err());
    assert!(error.contains("pre_resource_release"), "{error}");
    assert_eq!(
        manager
            .leases
            .list_resources(&*manager, Some(&selector))
            .await
            .unwrap()
            .len(),
        1
    );
}

/// A chat's config is its repository's, read from the registered checkout,
/// even for a chat in a workspace's worktree, whose file is the workspace's.
#[tokio::test]
async fn a_chats_layers_are_its_repositorys() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    std::fs::write(checkout.join(".shoal.toml"), "git_profile = 'checkout'\n").unwrap();
    std::fs::write(
        workspace.path.join(".shoal.toml"),
        "git_profile = 'worktree'\n",
    )
    .unwrap();
    let chat = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "reviewing".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();

    let read = |id: &shoal_ids::space_ids::SpaceId| {
        let target = crate::protocol::ConfigTarget::Space(SpaceSelector::id(id));
        manager.repositories.config_layers_for(&*manager, target)
    };
    let chat = read(&chat.id).await.unwrap();
    let workspace = read(&workspace.id).await.unwrap();
    assert_eq!(chat.worktree_file.git_profile.as_deref(), Some("checkout"));
    assert_eq!(
        workspace.worktree_file.git_profile.as_deref(),
        Some("worktree")
    );
}

/// A registered repository whose local config is `local`.
async fn repository_with_local_config(
    local: &str,
) -> (
    tempfile::TempDir,
    std::sync::Arc<crate::daemon::manager::Manager>,
    crate::model::Repository,
) {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = (manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    (manager.write_local_config(&repo.id, Some(local.into())))
        .await
        .unwrap();
    (root, manager, repo)
}

/// The names of the secrets the workspace `id`'s agents receive.
async fn secret_names(
    manager: &crate::daemon::manager::Manager,
    id: &shoal_ids::space_ids::SpaceId,
) -> Vec<String> {
    let selector = SpaceSelector::id(id);
    let catalog = manager
        .leases
        .secret_catalog(manager, &selector)
        .await
        .unwrap();
    (catalog.secrets.into_iter())
        .map(|secret| secret.name)
        .collect()
}

#[tokio::test]
async fn a_workspace_whose_file_lacks_the_checkouts_trigger_still_never_gets_its_secret() {
    let secrets = "[secrets.HOOK]\nvalue = { cmd = ['printf', 's3cret'] }\n[secrets.API_KEY]\nvalue = { cmd = ['true'] }\n";
    let (_root, manager, repo) = repository_with_local_config(secrets).await;
    let workspace = (manager.create_workspace(&repo.id, "fix".into(), None, None, None))
        .await
        .unwrap();
    // A trigger the checkout's file gained after the workspace branched.
    std::fs::write(
        repo.path.join(".shoal.toml"),
        "[triggers.labelled]\non = 'webhook'\nsecret = 'HOOK'\nsignature = 'forgejo'\n",
    )
    .unwrap();

    assert_eq!(secret_names(&manager, &workspace.id).await, ["API_KEY"]);
}

#[tokio::test]
async fn an_unreadable_checkout_file_withholds_every_local_secret() {
    let local = "[secrets.HOOK]\nvalue = { cmd = ['true'] }\n";
    let (_root, manager, repo) = repository_with_local_config(local).await;
    test_support::configure(&manager, "[secrets.API_KEY]\nvalue = { cmd = ['true'] }\n").await;
    let workspace = (manager.create_workspace(&repo.id, "fix".into(), None, None, None))
        .await
        .unwrap();
    // Its triggers could sign with any of the local config's secrets.
    std::fs::write(repo.path.join(".shoal.toml"), "[triggers.labelled\n").unwrap();

    assert_eq!(secret_names(&manager, &workspace.id).await, ["API_KEY"]);
}
