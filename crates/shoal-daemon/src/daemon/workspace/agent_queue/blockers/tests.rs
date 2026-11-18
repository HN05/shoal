use super::*;
use crate::{
    daemon::{
        test_support::{commit, global_config, manager},
        workspace::agent_queue::tests::{admitted, create, launch_in, register, request, started},
        workspace_files::Attacher,
    },
    model::{QueueEntry, QueueRequest, Repository},
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::BranchName;

/// Queue `name` blocked by `blockers` in a repository without agent limits.
async fn add_blocked(
    manager: &Manager,
    repository: &Repository,
    name: &str,
    blockers: Vec<Blocker>,
) -> QueueEntry {
    let admission = manager
        .queue_add(QueueRequest {
            blockers,
            ..request(manager, repository, name, false).await
        })
        .await
        .unwrap();
    assert!(!admitted(&admission), "{name} waits for its blockers");
    admission.entry.unwrap()
}

async fn entry(manager: &Manager, id: &QueueEntryId) -> QueueEntry {
    manager
        .queue_entries()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| &entry.id == id)
        .unwrap()
}

async fn space(manager: &Manager, name: &str) -> Blocker {
    Blocker::Space(manager.queued(name).await)
}

async fn remove(manager: &Manager, workspace: &Workspace) {
    manager
        .remove_selected(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn launches_start_on_a_blocker_once_it_is_done_at_its_head() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[machine]\nmax_heavy_agents = 'none'"));
    let api = create(&manager, &repo, "api").await;
    // Without a limit a launch that waits for other work still queues.
    let ui = add_blocked(&manager, &repo, "ui", vec![space(&manager, "api").await]).await;
    assert_eq!(ui.blockers, ["api"]);

    manager
        .mark_space_done(
            &SpaceSelector::id(&api.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    std::fs::write(api.path.join("more"), "more\n").unwrap();
    commit(&api.path, "more");
    // A commit after completion is work in progress again.
    manager.dispatch_queue().await.unwrap();
    assert!(!started(&ui.id));
    assert_eq!(entry(&manager, &ui.id).await.blockers, ["api"]);

    manager
        .mark_space_done(
            &SpaceSelector::id(&api.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&ui.id));
    let ui = entry(&manager, &ui.id).await;
    assert_eq!(
        ui.base.as_deref(),
        Some(api.branch().map(BranchName::as_str).unwrap())
    );
    assert!(ui.blockers.is_empty());
}

#[tokio::test]
async fn launches_blocked_by_a_chat_start_once_it_is_done_or_removed() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[machine]\nmax_heavy_agents = 'none'"));
    let notes =
        crate::daemon::test_support::chat(&manager, "notes", "chat", Some(&repo), None).await;
    let blocker = Blocker::Space(notes.id.to_string());
    let ui = add_blocked(&manager, &repo, "ui", vec![blocker]).await;
    assert_eq!(ui.blockers, ["notes"]);

    manager
        .mark_space_done(
            &SpaceSelector::id(&notes.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&ui.id));
    // A chat has no branch to build on.
    assert_eq!(entry(&manager, &ui.id).await.base, None);

    // A removed chat no longer blocks.
    let talk = crate::daemon::test_support::chat(&manager, "talk", "chat", Some(&repo), None).await;
    let blocker = Blocker::Space(talk.id.to_string());
    let docs = add_blocked(&manager, &repo, "docs", vec![blocker]).await;
    manager.end_executions(&talk.id).await;
    manager
        .remove_selected(
            &SpaceSelector::id(&talk.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&docs.id));
}

#[tokio::test]
async fn a_launch_blocker_follows_the_workspace_it_opens_until_removal() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 1\n[machine.overload.disk]\nenabled = false",
    ));
    let opened = create(&manager, &repo, "first").await;
    let first = manager
        .queue_add(launch_in(&opened, false))
        .await
        .unwrap()
        .entry
        .unwrap();
    let second = add_blocked(
        &manager,
        &repo,
        "second",
        vec![space(&manager, "first").await],
    )
    .await;
    assert_eq!(second.blockers, ["first"]);

    manager
        .queue_attach(&first.id, &SpaceSelector::id(&opened.id))
        .await
        .unwrap();
    assert_eq!(entry(&manager, &second.id).await.blockers, ["first"]);
    // A removed blocker leaves nothing to wait for, so the launch starts
    // from the default branch in the slot the removal freed.
    remove(&manager, &opened).await;
    manager.dispatch_queue().await.unwrap();
    assert!(started(&second.id));
    assert_eq!(entry(&manager, &second.id).await.base, None);
}

#[tokio::test]
async fn issue_blockers_wait_for_closure_or_a_done_workspace_linking_them() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[machine]\nmax_heavy_agents = 'none'"));
    let (closed, linked) = (
        "https://forge.example/team/repo/issues/1",
        "https://forge.example/team/repo/issues/2",
    );
    let blockers = vec![
        Blocker::Issue(closed.parse().unwrap()),
        Blocker::Issue(linked.parse().unwrap()),
    ];
    let launch = add_blocked(&manager, &repo, "launch", blockers).await;
    assert_eq!(launch.blockers, [closed, linked]);

    let cleared = crate::daemon::blockers::Lookup::Cleared("it closed".into());
    let lookups = [((BlockerKind::Issue, closed.to_owned()), cleared)].into();
    manager
        .forget_cleared_launch_blockers(&lookups)
        .await
        .unwrap();
    // Several workspaces may link the issue; any one done completes it.
    let fix = create(&manager, &repo, "fix").await;
    let pending = create(&manager, &repo, "pending").await;
    for workspace in [&fix, &pending] {
        let (id, url) = (workspace.id.clone(), linked.to_owned());
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO space_issue(space_id,url) VALUES (?1,?2)",
                    rusqlite::params![id, url],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
    assert_eq!(
        entry(&manager, &launch.id).await.blockers,
        ["fix or pending"]
    );
    manager
        .mark_space_done(
            &SpaceSelector::id(&fix.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(started(&launch.id));
    assert_eq!(
        entry(&manager, &launch.id).await.base.as_deref(),
        Some(fix.branch().map(BranchName::as_str).unwrap())
    );
}

#[tokio::test]
async fn done_blockers_build_on_the_one_that_contains_the_others() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[machine]\nmax_heavy_agents = 'none'"));
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
            lower.branch().map(ToString::to_string),
            None,
            None,
        )
        .await
        .unwrap();
    std::fs::write(upper.path.join("upper"), "work\n").unwrap();
    commit(&upper.path, "upper");
    let apart = add_blocked(
        &manager,
        &repo,
        "apart",
        vec![
            space(&manager, "lower").await,
            space(&manager, "separate").await,
        ],
    )
    .await;
    for workspace in [&lower, &separate, &upper] {
        manager
            .mark_space_done(
                &SpaceSelector::id(&workspace.id),
                Done::default(),
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    // Done blockers admit the launch at once, on the top one.
    let stacked = manager
        .queue_add(QueueRequest {
            blockers: vec![
                space(&manager, "lower").await,
                space(&manager, "upper").await,
            ],
            ..request(&manager, &repo, "stacked", false).await
        })
        .await
        .unwrap();
    assert!(admitted(&stacked));
    assert_eq!(
        stacked.entry.unwrap().base.as_deref(),
        Some(upper.branch().map(BranchName::as_str).unwrap())
    );
    manager.dispatch_queue().await.unwrap();
    // Separate branches wait until all but one have merged and gone.
    assert!(!started(&apart.id));
    assert_eq!(
        entry(&manager, &apart.id).await.blockers,
        ["lower", "separate"]
    );
    remove(&manager, &separate).await;
    manager.dispatch_queue().await.unwrap();
    assert!(started(&apart.id));
}

#[tokio::test]
async fn blockers_name_existing_work_other_than_the_launch() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[machine]\nmax_heavy_agents = 'none'"));
    let blockers = [
        space(&manager, "missing").await,
        space(&manager, "self").await,
    ];
    let own = create(&manager, &repo, "self").await;
    for blocker in blockers {
        manager
            .queue_add(QueueRequest {
                blockers: vec![blocker],
                ..launch_in(&own, false)
            })
            .await
            .unwrap_err();
    }
    assert!(manager.queue_entries().await.unwrap().is_empty());
    // The CLI checks them before any prompt.
    let selector = crate::repository::Selector::from(&repo.id);
    manager
        .check_blockers(&selector, vec![space(&manager, "missing").await])
        .await
        .unwrap_err();
    create(&manager, &repo, "present").await;
    manager
        .check_blockers(&selector, vec![space(&manager, "present").await])
        .await
        .unwrap();
    // Starting now cannot wait for work that is not done.
    manager
        .queue_add(QueueRequest {
            blockers: vec![space(&manager, "present").await],
            ..request(&manager, &repo, "now", true).await
        })
        .await
        .unwrap_err();
}

#[test]
fn the_setup_of_an_entrys_workspace_shows_once_its_other_blockers_are_done() {
    let waiting = |label: &str| Progress::Waiting(label.into());
    let labels = |progress: Vec<(BlockerKind, Progress)>| -> Vec<String> {
        (setup_last(progress).into_iter())
            .filter_map(|progress| match progress {
                Progress::Waiting(label) => Some(label),
                _ => None,
            })
            .collect()
    };
    let setup = || (BlockerKind::Setup, waiting("setup of docs"));
    // Its setup waits for the space it is blocked by, so only that shows.
    assert_eq!(
        labels(vec![(BlockerKind::Space, waiting("fix-login")), setup()]),
        ["fix-login"]
    );
    // Once that is done, the setup is what it waits for.
    assert_eq!(
        labels(vec![(BlockerKind::Space, Progress::Complete), setup()]),
        ["setup of docs"]
    );
    assert_eq!(labels(vec![setup()]), ["setup of docs"]);
}
