use super::*;

#[tokio::test]
async fn reload_replaces_the_global_config_only_when_valid() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let before = manager.config();
    crate::test_support::write_global_config(
        &manager.paths,
        "[machine.simulators.profiles.phone]\nruntime = 'iOS 27'\n",
    );
    assert!(manager.reload_config().await.is_err());
    assert!(Arc::ptr_eq(&manager.config(), &before));
    crate::test_support::write_global_config(
        &manager.paths,
        "[machine.simulators]\ndefault = 'phone'\n\
             [machine.simulators.profiles.phone]\ndevice = 'iPhone 17'\n",
    );
    manager.reload_config().await.unwrap();
    assert_eq!(
        manager.config().machine.simulators.default.as_deref(),
        Some("phone")
    );
    // A snapshot taken before the reload keeps the settings it started with.
    assert_eq!(before.machine.simulators.default, None);
}

#[tokio::test]
async fn existing_base_falls_back_only_when_the_default_ref_is_missing() {
    let (root, manager) = crate::daemon::test_support::manager().await;
    let path = crate::daemon::test_support::repository(root.path(), "repo");
    // The cached remote HEAD names the default branch without reading it.
    crate::daemon::test_support::git(&path, &["switch", "-c", "topic"]);
    crate::daemon::test_support::git(&path, &["remote", "add", "origin", "/missing"]);
    crate::daemon::test_support::git(
        &path,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let topic = crate::daemon::test_support::git(&path, &["rev-parse", "topic"]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    assert_eq!(
        existing_base(&repo, "topic").await.unwrap(),
        "refs/heads/main"
    );
    let main = path.join(".git/refs/heads/main");
    fs::write(&main, "garbage\n").unwrap();
    assert!(existing_base(&repo, "topic").await.is_err());
    fs::remove_file(&main).unwrap();
    assert_eq!(existing_base(&repo, "topic").await.unwrap(), topic.trim());
    manager.store.shutdown().await;
}

#[tokio::test]
async fn records_report_only_running_executions() {
    let (root, manager) = crate::daemon::test_support::manager().await;
    let path = crate::daemon::test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "busy".into(), None, None, None)
        .await
        .unwrap();
    assert!(
        !manager
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .running
    );
    for (state, running) in [("running", true), ("unknown", false)] {
        let id = workspace.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT OR REPLACE INTO executions(id,workspace_id,state) VALUES ('run',?1,?2)",
                    [&id, state],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            manager
                .workspace_by_id(&workspace.id)
                .await
                .unwrap()
                .running,
            running
        );
        assert_eq!(manager.list_workspaces().await.unwrap()[0].running, running);
    }
    manager.store.shutdown().await;
}
