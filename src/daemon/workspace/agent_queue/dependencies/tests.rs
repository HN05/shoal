use super::*;
use crate::{
    daemon::workspace::agent_queue::tests::{create, register, request, started},
    model::{QueueEntry, QueueRequest, Repository},
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{commit, global_config, manager},
};

/// Queue `name` after `after` in a repository without agent limits.
async fn add_after(
    manager: &Manager,
    repository: &Repository,
    name: &str,
    after: Vec<Dependency>,
) -> QueueEntry {
    let admission = manager
        .queue_add(QueueRequest {
            after,
            ..request(repository, name, false)
        })
        .await
        .unwrap();
    assert!(!admission.start, "{name} waits for its dependencies");
    admission.entry.unwrap()
}

async fn entry(manager: &Manager, id: &str) -> QueueEntry {
    manager
        .queue_entries()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.id == id)
        .unwrap()
}

fn work(name: &str) -> Dependency {
    Dependency::Work(name.into())
}

async fn remove(manager: &Manager, workspace: &Workspace) {
    manager
        .remove_workspace(
            &workspace.id,
            UnsavedWork::default(),
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn launches_start_on_a_dependency_once_it_is_done_at_its_head() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("max_agents = 'none'"));
    let api = create(&manager, &repo, "api").await;
    // Without a limit a launch that waits for other work still queues.
    let ui = add_after(&manager, &repo, "ui", vec![work("api")]).await;
    assert_eq!(ui.waiting_for, ["api"]);

    manager.mark_done(&api.id, false, false).await.unwrap();
    std::fs::write(api.path.join("more"), "more\n").unwrap();
    commit(&api.path, "more");
    // A commit after completion is work in progress again.
    manager.dispatch_queue().await.unwrap();
    assert!(!started(&ui.id));
    assert_eq!(entry(&manager, &ui.id).await.waiting_for, ["api"]);

    manager.mark_done(&api.id, false, false).await.unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&ui.id));
    let ui = entry(&manager, &ui.id).await;
    assert_eq!(ui.base.as_deref(), Some(api.branch.as_str()));
    assert!(ui.waiting_for.is_empty());
}

#[tokio::test]
async fn a_launch_dependency_follows_the_workspace_it_opens_until_removal() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config(
        "max_agents = 1\n[overload.disk]\nenabled = false",
    ));
    let first = manager
        .queue_add(request(&repo, "first", false))
        .await
        .unwrap()
        .entry
        .unwrap();
    let second = add_after(&manager, &repo, "second", vec![work("first")]).await;
    assert_eq!(second.waiting_for, ["first"]);

    let opened = create(&manager, &repo, "first").await;
    manager.queue_attach(&first.id, &opened.id).await.unwrap();
    assert_eq!(entry(&manager, &second.id).await.waiting_for, ["first"]);
    // A removed dependency leaves nothing to wait for, so the launch starts
    // from the default branch in the slot the removal freed.
    remove(&manager, &opened).await;
    manager.dispatch_queue().await.unwrap();
    assert!(started(&second.id));
    assert_eq!(entry(&manager, &second.id).await.base, None);
}

#[tokio::test]
async fn issue_dependencies_wait_for_closure_or_a_done_workspace_linking_them() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("max_agents = 'none'"));
    let (closed, linked) = (
        "https://forge.example/team/repo/issues/1",
        "https://forge.example/team/repo/issues/2",
    );
    let after = vec![
        Dependency::Issue(closed.into()),
        Dependency::Issue(linked.into()),
    ];
    let launch = add_after(&manager, &repo, "launch", after).await;
    assert_eq!(launch.waiting_for, [closed, linked]);

    manager.forget_issue(closed.into()).await.unwrap();
    // Several workspaces may link the issue; any one done completes it.
    let fix = create(&manager, &repo, "fix").await;
    let pending = create(&manager, &repo, "pending").await;
    for workspace in [&fix, &pending] {
        let (id, url) = (workspace.id.clone(), linked.to_owned());
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO workspace_issue(workspace_id,url) VALUES (?1,?2)",
                    [id, url],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
    assert_eq!(
        entry(&manager, &launch.id).await.waiting_for,
        ["fix or pending"]
    );
    manager.mark_done(&fix.id, false, false).await.unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&launch.id));
    assert_eq!(
        entry(&manager, &launch.id).await.base.as_deref(),
        Some(fix.branch.as_str())
    );
}

#[tokio::test]
async fn done_dependencies_build_on_the_one_that_contains_the_others() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("max_agents = 'none'"));
    let (lower, separate) = (
        create(&manager, &repo, "lower").await,
        create(&manager, &repo, "separate").await,
    );
    for workspace in [&lower, &separate] {
        std::fs::write(workspace.path.join(&workspace.name), "work\n").unwrap();
        commit(&workspace.path, &workspace.name);
    }
    let upper = manager
        .create_workspace(
            &repo.id,
            "upper".into(),
            Some(lower.branch.clone()),
            None,
            None,
        )
        .await
        .unwrap();
    std::fs::write(upper.path.join("upper"), "work\n").unwrap();
    commit(&upper.path, "upper");
    let apart = add_after(
        &manager,
        &repo,
        "apart",
        vec![work("lower"), work("separate")],
    )
    .await;
    for workspace in [&lower, &separate, &upper] {
        manager
            .mark_done(&workspace.id, false, false)
            .await
            .unwrap();
    }
    // Complete dependencies start the launch in the caller, on the top one.
    let stacked = manager
        .queue_add(QueueRequest {
            after: vec![work("lower"), work("upper")],
            ..request(&repo, "stacked", false)
        })
        .await
        .unwrap();
    assert!(stacked.start);
    assert_eq!(
        stacked.entry.unwrap().base.as_deref(),
        Some(upper.branch.as_str())
    );
    manager.dispatch_queue().await.unwrap();
    // Separate branches wait until all but one have merged and gone.
    assert!(!started(&apart.id));
    assert_eq!(
        entry(&manager, &apart.id).await.waiting_for,
        ["lower", "separate"]
    );
    remove(&manager, &separate).await;
    manager.dispatch_queue().await.unwrap();
    assert!(started(&apart.id));
}

#[tokio::test]
async fn dependencies_name_existing_work_other_than_the_launch() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("max_agents = 'none'"));
    for after in [work("missing"), work("self")] {
        let error = manager
            .queue_add(QueueRequest {
                after: vec![after],
                ..request(&repo, "self", false)
            })
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("neither a workspace nor a queued agent"));
    }
    assert!(manager.queue_entries().await.unwrap().is_empty());
    // The CLI checks them before any prompt.
    let selector = crate::repository::Selector::from(repo.id.as_str());
    let error = manager
        .check_dependencies(&selector, vec![work("missing")])
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("neither a workspace nor a queued agent"));
    create(&manager, &repo, "present").await;
    manager
        .check_dependencies(&selector, vec![work("present")])
        .await
        .unwrap();
    let error = manager
        .queue_add(QueueRequest {
            after: vec![work("missing")],
            ..request(&repo, "now", true)
        })
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("cannot wait"));
}
