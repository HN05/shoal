use super::*;
use crate::{
    daemon::workspace_files::Attacher,
    forge::updates::UpdateKind,
    test_support::{manager, repository},
};
use serde_json::{Value, json};

#[test]
fn a_rate_limited_poll_waits_for_the_limit_but_at_least_the_interval() {
    let limit = |until| {
        anyhow::Error::new(crate::forge::transport::limits::RateLimited {
            host: "forge.example".into(),
            until,
        })
        .context("GET issues/8")
    };
    let (updates, interval) = limited_poll(Err(limit(1_600)), 1_000).unwrap();
    assert!(updates.is_empty());
    assert_eq!(interval, Duration::from_secs(600));
    let (_, interval) = limited_poll(Err(limit(1_001)), 1_000).unwrap();
    assert_eq!(interval, POLL_INTERVAL);
    let error = limited_poll(Err(anyhow::anyhow!("HTTP 500")), 1_000).unwrap_err();
    assert_eq!(format!("{error:#}"), "HTTP 500");
}

#[tokio::test(start_paused = true)]
async fn a_rate_limited_watch_keeps_polling_after_the_limit() {
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let result = wait_for_updates(None, &mut tokio::sync::watch::channel(0).1, || async {
        match attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) {
            0 => Err(anyhow::Error::new(
                crate::forge::transport::limits::RateLimited {
                    host: "forge.example".into(),
                    until: 0,
                },
            )),
            _ => Err(anyhow::anyhow!("HTTP 500")),
        }
    })
    .await;
    assert_eq!(format!("{:#}", result.unwrap_err()), "HTTP 500");
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
}

#[tokio::test(start_paused = true)]
async fn polling_wakes_for_a_single_result_and_timeout_has_no_update() {
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let started = tokio::time::Instant::now();
    let result = wait_for_updates(
        watch_deadline(Some(3600)),
        &mut tokio::sync::watch::channel(0).1,
        || async {
            Ok(
                if attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                    Vec::new()
                } else {
                    vec![Update {
                        url: "pr".into(),
                        kind: UpdateKind::CiCompleted,
                        message: "review passed".into(),
                        delivery: String::new(),
                        check: None,
                        workspace: None,
                    }]
                },
            )
        },
    )
    .await
    .unwrap();
    assert!(!result.timed_out);
    assert_eq!(result.updates.len(), 1);
    assert_eq!(tokio::time::Instant::now() - started, POLL_INTERVAL);
    let started = tokio::time::Instant::now();
    let result = wait_for_updates(
        watch_deadline(Some(75)),
        &mut tokio::sync::watch::channel(0).1,
        || async { Ok(Vec::new()) },
    )
    .await
    .unwrap();
    assert!(result.timed_out && result.updates.is_empty());
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(75)
    );
    let error = wait_for_updates(
        watch_deadline(Some(3600)),
        &mut tokio::sync::watch::channel(0).1,
        || async { anyhow::bail!("lookup failed") },
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "lookup failed");
}

#[tokio::test(start_paused = true)]
async fn queued_background_updates_wake_a_wait_before_its_next_poll() {
    let (queue, mut queued) = tokio::sync::watch::channel(0);
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let started = tokio::time::Instant::now();
    let wait = wait_for_updates(None, &mut queued, || async {
        Ok(
            if attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                queue.send_modify(|count| *count += 1);
                Vec::new()
            } else {
                vec![Update {
                    url: "pr".into(),
                    kind: UpdateKind::Comment,
                    message: "queued".into(),
                    delivery: String::new(),
                    check: None,
                    workspace: None,
                }]
            },
        )
    });
    assert_eq!(wait.await.unwrap().updates[0].message, "queued");
    assert_eq!(tokio::time::Instant::now(), started);
}

#[tokio::test(start_paused = true)]
async fn polling_without_a_timeout_stays_unbounded() {
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let started = tokio::time::Instant::now();
    let result = wait_for_updates(None, &mut tokio::sync::watch::channel(0).1, || async {
        if attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 120 {
            Ok(vec![Update {
                url: "pr".into(),
                kind: UpdateKind::CiCompleted,
                message: "review passed".into(),
                delivery: String::new(),
                check: None,
                workspace: None,
            }])
        } else {
            Ok(Vec::new())
        }
    })
    .await
    .unwrap();
    assert!(!result.timed_out);
    assert_eq!(result.updates.len(), 1);
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(3600)
    );
}

#[tokio::test]
async fn a_newer_wait_supersedes_the_older_one_per_workspace() {
    let waits = ActiveWaits::default();
    let older = waits.start("one");
    let other = waits.start("two");
    let newer = waits.start("one");
    older.superseded.notified().await;
    assert!(!older.is_current());
    assert!(newer.is_current() && other.is_current());
    drop(older);
    assert!(newer.is_current());
    drop(newer);
    assert!(!waits.0.lock().unwrap().contains_key("one"));
}

#[tokio::test]
async fn agent_messages_end_a_watch_before_item_activity() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "messages".into(), None, None, None)
        .await
        .unwrap();
    // Waiting would fail without a linked PR; a queued message answers first.
    manager
        .send_agent_message(
            &workspace.id,
            "Rebase onto main".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let updates = manager.wait_prs(&workspace.id, Some(60)).await.unwrap();
    assert!(updates.updates.is_empty() && !updates.timed_out);
    assert_eq!(updates.messages[0].message, "Rebase onto main");
    manager
        .mark_agent_messages_delivered(&workspace.id, vec![updates.messages[0].id])
        .await
        .unwrap();
    assert!(manager.wait_prs(&workspace.id, Some(60)).await.is_err());

    let mut queued = manager.signals.watch_agent_messages();
    let waiting = {
        let manager = manager.clone();
        let id = workspace.id.clone();
        tokio::spawn(async move { manager.next_agent_messages(&id, &mut queued).await })
    };
    manager
        .send_agent_message(
            &workspace.id,
            "Stop the dev server".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let messages = waiting.await.unwrap().unwrap();
    assert_eq!(messages[0].message, "Stop the dev server");
}

fn snapshot(message: &str) -> Snapshot {
    serde_json::from_value(json!({"comments": {"one": message}, "checks": {}, "conflict": null, "state": "open", "errors": {}})).unwrap()
}

async fn register_watch(manager: &Manager, id: &str, url: &str) {
    let id = id.to_owned();
    let registration = super::super::pr::Registration {
        kind: RegistrationKind::Watch {
            urls: vec![url.into()],
            merged_head: None,
        },
        error: None,
    };
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT OR REPLACE INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                params![id, serde_json::to_string(&registration)?],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn background_activity_uses_its_own_baseline_and_publishes_events() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "activity".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;

    manager
        .record_background_activity(&workspace.id, vec![("pr".into(), snapshot("first"))])
        .await
        .unwrap();
    assert!(manager
            .workspace_events(None, 100)
            .await
            .unwrap()
            .iter()
            .all(|item| !matches!(item, crate::daemon::events::EventItem::Event(event) if event.details.kind == crate::daemon::events::EventKind::ItemActivity)));

    manager
        .record_background_activity(&workspace.id, vec![("pr".into(), snapshot("second"))])
        .await
        .unwrap();
    let events = manager.workspace_events(None, 100).await.unwrap();
    let activity = events.iter().find_map(|item| match item {
        crate::daemon::events::EventItem::Event(event)
            if event.details.kind == crate::daemon::events::EventKind::ItemActivity =>
        {
            event.details.item_activity.as_deref()
        }
        _ => None,
    });
    let activity = activity.expect("background activity event");
    assert_eq!(activity.kind, LinkKind::Pr);
    assert_eq!(activity.url, "pr");
    assert_eq!(activity.update.kind, UpdateKind::Comment);
}

#[tokio::test]
async fn background_activity_queues_for_the_agents_hooks() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "queued".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    let queued = manager.signals.watch_item_updates();
    manager
        .queue_background_updates(
            &workspace.id,
            vec![("pr".into(), snapshot("first"))],
            crate::time::unix_seconds(),
        )
        .await
        .unwrap();
    assert!(queued.has_changed().unwrap());
    manager
        .send_agent_message(
            &workspace.id,
            "Rebase onto main".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // Handing them to the agent's hook replaces the running watch.
    let watch = manager.item_waits.start(&workspace.id);
    let delivered = manager.agent_updates(&workspace.id).await.unwrap();
    watch.superseded.notified().await;
    assert!(!watch.is_current());
    assert_eq!(delivered.updates[0].kind, UpdateKind::Comment);
    assert_eq!(delivered.messages[0].message, "Rebase onto main");
    manager
        .mark_agent_messages_delivered(&workspace.id, vec![delivered.messages[0].id])
        .await
        .unwrap();
    manager
        .acknowledge_pr_updates(&workspace.id, vec![delivered.updates[0].delivery.clone()])
        .await
        .unwrap();
    let after = manager.agent_updates(&workspace.id).await.unwrap();
    assert!(after.updates.is_empty() && after.messages.is_empty());
}

#[tokio::test]
async fn a_chats_hooks_receive_the_activity_on_its_items() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    register_watch(&manager, &chat.id, "pr").await;
    let manager = std::sync::Arc::new(manager);
    // The chat's wake hook waits for the activity the poll queues.
    let waiting = tokio::spawn({
        let (manager, chat) = (manager.clone(), chat.id.clone());
        async move { manager.wait_chat_messages(&chat).await }
    });
    manager
        .queue_background_updates(
            &chat.id,
            vec![("pr".into(), snapshot("first"))],
            crate::time::unix_seconds(),
        )
        .await
        .unwrap();
    let woken = waiting.await.unwrap().unwrap();
    assert_eq!(woken.updates[0].kind, UpdateKind::Comment);
    // A watch in the chat yields to its hook's delivery, which hands over
    // the same update once acknowledged.
    let watch = manager.chat_waits.start(&chat.id);
    let delivered = manager.chat_updates(&chat.id).await.unwrap();
    watch.superseded.notified().await;
    assert!(!watch.is_current());
    assert_eq!(delivered.updates, woken.updates);
    manager
        .acknowledge_pr_updates(&chat.id, vec![delivered.updates[0].delivery.clone()])
        .await
        .unwrap();
    assert!(
        manager
            .chat_updates(&chat.id)
            .await
            .unwrap()
            .updates
            .is_empty()
    );
}

#[tokio::test]
async fn a_chat_receives_the_activity_of_the_workspaces_below_it_without_an_agent() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let worker = manager
        .create_workspace_below(
            repo.id.clone().into(),
            "worker".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    register_watch(&manager, &worker.id, "worker-pr").await;
    manager
        .queue_background_updates(
            &worker.id,
            vec![("worker-pr".into(), snapshot("first"))],
            crate::time::unix_seconds(),
        )
        .await
        .unwrap();
    let delivered = manager.chat_updates(&chat.id).await.unwrap();
    assert_eq!(delivered.updates.len(), 1);
    assert_eq!(delivered.updates[0].workspace.as_deref(), Some("worker"));
    let deliveries = vec![delivered.updates[0].delivery.clone()];
    let pending = || async { manager.pending_pr_activity(&worker.id).await.unwrap().len() };
    // A newer watch in the workspace below keeps what the chat acknowledges.
    let worker_wait = manager.item_waits.start(&worker.id);
    manager
        .acknowledge_pr_updates(&chat.id, deliveries.clone())
        .await
        .unwrap();
    assert_eq!(pending().await, 1);
    drop(worker_wait);
    // And a newer wait in the chat keeps what the workspace acknowledges.
    let chat_wait = manager.chat_waits.start(&chat.id);
    manager
        .acknowledge_pr_updates(&worker.id, deliveries.clone())
        .await
        .unwrap();
    assert_eq!(pending().await, 1);
    drop(chat_wait);
    manager
        .acknowledge_pr_updates(&chat.id, deliveries)
        .await
        .unwrap();
    assert!(
        manager
            .chat_updates(&chat.id)
            .await
            .unwrap()
            .updates
            .is_empty()
    );
}

#[tokio::test]
async fn a_chats_wait_takes_activity_an_item_change_held_back() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    register_watch(&manager, &chat.id, "pr").await;
    let manager = std::sync::Arc::new(manager);
    // Activity queues, signalling before the wait starts, and then an item
    // change holds the chat's PR gate, so the wait finds nothing it may
    // take yet and no signal is left to wake it.
    manager
        .queue_background_updates(
            &chat.id,
            vec![("pr".into(), snapshot("first"))],
            crate::time::unix_seconds(),
        )
        .await
        .unwrap();
    let gate = manager.gates.lock_prs([&chat.id]).await;
    let waiting = tokio::spawn({
        let (manager, chat) = (manager.clone(), chat.id.clone());
        async move { manager.wait_chat_messages(&chat).await }
    });
    // The change ends without a signal once the wait found the gate held;
    // the wait looks again by itself.
    manager.gates.pr_contention.notified().await;
    drop(gate);
    let woken = tokio::time::timeout(std::time::Duration::from_secs(10), waiting)
        .await
        .expect("the wait slept past the released gate")
        .unwrap()
        .unwrap();
    assert_eq!(woken.updates[0].kind, UpdateKind::Comment);
}

#[tokio::test]
async fn hook_delivery_does_not_wait_for_the_pr_gate() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "gated".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    manager
        .queue_background_updates(
            &workspace.id,
            vec![("pr".into(), snapshot("first"))],
            crate::time::unix_seconds(),
        )
        .await
        .unwrap();
    manager
        .send_agent_message(
            &workspace.id,
            "Rebase onto main".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let queued = manager.agent_updates(&workspace.id).await.unwrap();
    let delivery = queued.updates[0].delivery.clone();
    // Cleanup holds the gate across forge requests and removals.
    let gate = manager.gates.lock_prs([&workspace.id]).await;
    let delivered = manager.agent_updates(&workspace.id).await.unwrap();
    assert!(delivered.updates.is_empty());
    assert_eq!(delivered.messages[0].message, "Rebase onto main");
    manager
        .acknowledge_pr_updates(&workspace.id, vec![delivery])
        .await
        .unwrap();
    drop(gate);
    let after = manager.agent_updates(&workspace.id).await.unwrap();
    assert!(after.updates.is_empty());
}

#[tokio::test]
async fn hook_delivery_waits_only_for_the_workspaces_it_reads() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None, Default::default())
        .await
        .unwrap();
    register_watch(&manager, &child.id, "child-pr").await;
    manager
        .queue_background_updates(
            &child.id,
            vec![("child-pr".into(), snapshot("first"))],
            crate::time::unix_seconds(),
        )
        .await
        .unwrap();
    let unrelated = manager.gates.lock_prs([&other.id]).await;
    let delivered = manager.agent_updates(&parent.id).await.unwrap();
    assert_eq!(delivered.updates.len(), 1);
    assert_eq!(delivered.updates[0].workspace.as_deref(), Some("child"));
    drop(unrelated);
    // The parent's agent receives the child's updates, so delivery waits
    // for work on the child.
    let read = manager.gates.lock_prs([&child.id]).await;
    let delivered = manager.agent_updates(&parent.id).await.unwrap();
    assert!(delivered.updates.is_empty());
    drop(read);
}

#[tokio::test]
async fn child_activity_reaches_the_parents_agent_until_the_child_has_its_own() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None, Default::default())
        .await
        .unwrap();
    let grandchild = manager
        .split_workspace(&child.id, "grandchild".into(), None, Default::default())
        .await
        .unwrap();
    let watched = [(&child, "child-pr"), (&grandchild, "grandchild-pr")];
    for (workspace, url) in watched {
        register_watch(&manager, &workspace.id, url).await;
    }
    // The parent links no PR, but its children do.
    let prs = Selection {
        kind: Some(LinkKind::Pr),
        input: None,
    };
    // A poll records the parent's own observations without refusing the kind.
    let quiet = manager.poll_item_activity(&parent.id, &prs).await.unwrap();
    assert!(quiet.is_empty());
    // Queued updates end a wait at once; the timeout only guards a hang.
    let watch = |selection: Selection| {
        let manager = &manager;
        let id = parent.id.clone();
        async move {
            tokio::time::timeout(
                Duration::from_secs(60),
                manager.wait_items(&id, &selection, None),
            )
            .await
            .expect("watch ended")
        }
    };
    for (workspace, url) in watched {
        manager
            .queue_background_updates(
                &workspace.id,
                vec![(url.into(), snapshot("first"))],
                crate::time::unix_seconds(),
            )
            .await
            .unwrap();
    }
    // The child's branch reports files changed on both sides beside its PR.
    let changed = branch_conflicts(
        Ok(crate::model::ConflictCheck {
            workspace_id: child.id.clone(),
            target: "main".into(),
            target_commit: "main".into(),
            head: "head".into(),
            conflicts: false,
            files: Vec::new(),
            changed_on_both_sides: vec!["version".into()],
        }),
        BranchReport::ChangedOnBothSides,
    );
    let observation = vec![(child.branch.clone().unwrap(), changed)];
    let now = crate::time::unix_seconds();
    let queued = manager.queue_background_updates(&child.id, observation, now);
    queued.await.unwrap();
    let delivered = manager.agent_updates(&parent.id).await.unwrap();
    let received: Vec<_> = delivered
        .updates
        .iter()
        .map(|update| (update.workspace.as_deref(), update.url.as_str()))
        .collect();
    assert_eq!(
        received,
        [
            (Some("child"), child.branch.as_deref().unwrap()),
            (Some("child"), "child-pr"),
            (Some("grandchild"), "grandchild-pr")
        ]
    );
    // An unfiltered watch of the parent reports them as well; one of PRs
    // leaves out the branch.
    let unfiltered = watch(Selection::default()).await.unwrap();
    assert_eq!(unfiltered.updates.len(), 3);
    let filtered = watch(prs.clone()).await.unwrap();
    assert_eq!(filtered.updates.len(), 2);

    // An agent of the child's own takes over the child's subtree.
    manager
        .set_agent_state(&child.id, crate::state::AgentState::Working, None, None)
        .await
        .unwrap();
    assert!(
        manager
            .agent_updates(&parent.id)
            .await
            .unwrap()
            .updates
            .is_empty()
    );
    assert!(watch(prs.clone()).await.is_err());
    let own = manager.agent_updates(&child.id).await.unwrap();
    assert_eq!(own.updates.len(), 3);
    assert_eq!(own.updates[0].workspace, None);
    assert_eq!(own.updates[1].workspace, None);
    assert_eq!(own.updates[2].workspace.as_deref(), Some("grandchild"));
    manager
        .acknowledge_pr_updates(
            &child.id,
            own.updates
                .iter()
                .map(|update| update.delivery.clone())
                .collect(),
        )
        .await
        .unwrap();
    assert!(
        manager
            .agent_updates(&child.id)
            .await
            .unwrap()
            .updates
            .is_empty()
    );
    assert!(
        manager
            .agent_updates(&grandchild.id)
            .await
            .unwrap()
            .updates
            .is_empty()
    );
}

#[tokio::test]
async fn acknowledging_child_updates_leaves_them_to_a_running_child_wait() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None, Default::default())
        .await
        .unwrap();
    for (workspace, url) in [(&parent, "parent-pr"), (&child, "child-pr")] {
        register_watch(&manager, &workspace.id, url).await;
        manager
            .queue_background_updates(
                &workspace.id,
                vec![(url.into(), snapshot("first"))],
                crate::time::unix_seconds(),
            )
            .await
            .unwrap();
    }
    let delivered = manager.agent_updates(&parent.id).await.unwrap();
    assert_eq!(delivered.updates.len(), 2);
    let deliveries = delivered
        .updates
        .iter()
        .map(|update| update.delivery.clone())
        .collect::<Vec<_>>();
    // A newer watch in the parent receives the child's updates as well.
    let parent_wait = manager.item_waits.start(&parent.id);
    manager
        .acknowledge_pr_updates(&parent.id, deliveries.clone())
        .await
        .unwrap();
    assert_eq!(
        manager.pending_pr_activity(&parent.id).await.unwrap().len(),
        1
    );
    assert_eq!(
        manager.pending_pr_activity(&child.id).await.unwrap().len(),
        1
    );
    drop(parent_wait);
    // A watch in the child starts before the parent acknowledges.
    let child_wait = manager.item_waits.start(&child.id);
    manager
        .acknowledge_pr_updates(&parent.id, deliveries.clone())
        .await
        .unwrap();
    assert!(
        manager
            .pending_pr_activity(&parent.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        manager.pending_pr_activity(&child.id).await.unwrap().len(),
        1
    );
    drop(child_wait);
    // A watch in the child itself, acknowledged while a newer parent
    // watch runs, leaves the parent's watch the child's updates.
    let parent_wait = manager.item_waits.start(&parent.id);
    manager
        .acknowledge_pr_updates(&child.id, deliveries.clone())
        .await
        .unwrap();
    assert_eq!(
        manager.pending_pr_activity(&child.id).await.unwrap().len(),
        1
    );
    drop(parent_wait);
    manager
        .acknowledge_pr_updates(&parent.id, deliveries)
        .await
        .unwrap();
    assert!(
        manager
            .pending_pr_activity(&child.id)
            .await
            .unwrap()
            .is_empty()
    );
}

fn large_update(n: usize, delivery: usize) -> Update {
    Update {
        url: "child-pr".into(),
        kind: UpdateKind::Comment,
        message: format!("{n}{}", "x".repeat(4096)),
        delivery: delivery.to_string(),
        check: None,
        workspace: None,
    }
}

#[test]
fn combined_updates_fit_one_response_in_whole_deliveries() {
    const LIMIT: usize = crate::protocol::MAX_FRAME - 256;
    let batch = |deliveries: fn(usize) -> usize| {
        (0..63)
            .map(|n| ("child".to_owned(), large_update(n, deliveries(n))))
            .collect::<Batch>()
    };
    // 21 deliveries of three updates each, more than one response holds.
    let Cut::Between(fitting) = frame_cut(&batch(|n| n / 3), LIMIT) else {
        panic!("whole deliveries fit");
    };
    assert!(fitting > 0 && fitting < 63 && fitting % 3 == 0);
    let sent = batch(|n| n / 3)[..fitting].to_vec();
    assert!(serde_json::to_vec(&sent).unwrap().len() < crate::protocol::MAX_FRAME);
    // One delivery larger than a response is cut inside it.
    let Cut::Inside(fitting) = frame_cut(&batch(|_| 0), LIMIT) else {
        panic!("a delivery too large for a response is cut");
    };
    assert!(fitting > 0 && fitting < 63);
    assert_eq!(frame_cut(&Vec::new(), LIMIT), Cut::Between(0));
}

#[tokio::test]
async fn hook_responses_fit_messages_and_updates_together() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None, Default::default())
        .await
        .unwrap();
    register_watch(&manager, &child.id, "child-pr").await;
    let id = child.id.clone();
    manager
        .store
        .run(move |db| {
            for n in 0..63 {
                queue_update(db, &id, large_update(n, n))?;
            }
            Ok(())
        })
        .await
        .unwrap();
    // As many unread messages as a workspace holds.
    for n in 0..50 {
        manager
            .send_agent_message(
                &parent.id,
                format!("{n:02}{}", "m".repeat(498)),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    let delivered = manager.agent_updates(&parent.id).await.unwrap();
    assert_eq!(delivered.messages.len(), 50);
    assert!(!delivered.updates.is_empty());
    let response = crate::protocol::Response::new(1, crate::protocol::Body::PrUpdates(delivered));
    assert!(serde_json::to_vec(&response).unwrap().len() < crate::protocol::MAX_FRAME);
}

#[tokio::test]
async fn a_delivery_too_large_for_one_response_arrives_in_parts() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None, Default::default())
        .await
        .unwrap();
    register_watch(&manager, &child.id, "child-pr").await;
    let id = child.id.clone();
    manager
        .store
        .run(move |db| {
            for n in 0..63 {
                queue_update(db, &id, large_update(n, 0))?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let mut received = Vec::new();
    while received.len() < 63 {
        let delivered = manager.agent_updates(&parent.id).await.unwrap();
        assert!(!delivered.updates.is_empty() && delivered.updates.len() < 63);
        assert!(serde_json::to_vec(&delivered).unwrap().len() < crate::protocol::MAX_FRAME,);
        manager
            .acknowledge_pr_updates(
                &parent.id,
                delivered
                    .updates
                    .iter()
                    .map(|update| update.delivery.clone())
                    .collect(),
            )
            .await
            .unwrap();
        received.extend(delivered.updates);
    }
    let order: Vec<_> = (0..63).map(|n| n.to_string()).collect();
    for (update, n) in received.iter().zip(&order) {
        assert!(update.message.starts_with(&format!("{n}x")), "{n}");
    }
    assert!(
        manager
            .pending_pr_activity(&child.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn deliveries_left_out_of_a_response_stay_pending_whole() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None, Default::default())
        .await
        .unwrap();
    register_watch(&manager, &child.id, "child-pr").await;
    let id = child.id.clone();
    manager
        .store
        .run(move |db| {
            for n in 0..63 {
                queue_update(db, &id, large_update(n, n / 3))?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let first = manager.agent_updates(&parent.id).await.unwrap();
    assert!(first.updates.len() < 63);
    manager
        .acknowledge_pr_updates(
            &parent.id,
            first
                .updates
                .iter()
                .map(|update| update.delivery.clone())
                .collect(),
        )
        .await
        .unwrap();
    let rest = manager.pending_pr_activity(&child.id).await.unwrap();
    assert_eq!(first.updates.len() + rest.len(), 63);
    assert_eq!(rest.len() % 3, 0);
    assert!(
        rest[0]
            .message
            .starts_with(&first.updates.len().to_string())
    );
}

#[tokio::test]
async fn agent_wakes_are_due_while_an_idle_agent_has_updates() {
    let (root, manager) = manager().await;
    let manager = Arc::new(manager);
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "wake".into(), None, None, None)
        .await
        .unwrap();
    let wait = |due| {
        let manager = manager.clone();
        let id = workspace.id.clone();
        tokio::spawn(async move { manager.wait_agent_wake(&id, due, None).await })
    };
    // A hang guard only: each wait ends on the change before it.
    let finish = |task: tokio::task::JoinHandle<Result<()>>| async {
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .expect("wait ended")
            .unwrap()
            .unwrap();
    };
    let state = |state| manager.set_agent_state(&workspace.id, state, None, None);
    state(crate::state::AgentState::Working).await.unwrap();
    manager
        .send_agent_message(
            &workspace.id,
            "Rebase onto main".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let due = wait(true);
    state(crate::state::AgentState::Idle).await.unwrap();
    finish(due).await;
    let taken = wait(false);
    let messages = manager.agent_messages(&workspace.id).await.unwrap();
    manager
        .mark_agent_messages_delivered(&workspace.id, vec![messages[0].id])
        .await
        .unwrap();
    finish(taken).await;
}

/// An open PR at `head` with `checks` by name and state, `pending` unfinished.
pub(super) fn checks_at(head: &str, checks: &[(&str, &str)]) -> Snapshot {
    let checks = checks
        .iter()
        .map(|(name, state)| {
            let check = json!({"name": name, "state": state, "revision": head, "complete": *state != "pending"});
            ((*name).to_owned(), check)
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::from_value(json!({
        "comments": {}, "checks": checks, "head": head,
        "conflict": null, "state": "open", "errors": {}
    }))
    .unwrap()
}

#[tokio::test]
async fn an_idle_agent_wakes_once_for_a_heads_passing_checks() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "checks".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    let poll = async |snapshot| {
        manager
            .queue_background_updates(
                &workspace.id,
                vec![("pr".into(), snapshot)],
                crate::time::unix_seconds(),
            )
            .await
            .unwrap();
        manager.agent_updates(&workspace.id).await.unwrap().updates
    };
    let pending = [("fmt", "pending"), ("lint", "pending"), ("test", "pending")];
    assert!(poll(checks_at("a", &pending)).await.is_empty());
    let one = [("fmt", "success"), ("lint", "pending"), ("test", "pending")];
    assert!(poll(checks_at("a", &one)).await.is_empty());
    // A push drops the old head's passes.
    assert!(poll(checks_at("b", &pending)).await.is_empty());
    let two = [("fmt", "success"), ("lint", "success"), ("test", "pending")];
    assert!(poll(checks_at("b", &two)).await.is_empty());
    let all = [("fmt", "success"), ("lint", "success"), ("test", "success")];
    let updates = poll(checks_at("b", &all)).await;
    assert_eq!(
        updates
            .iter()
            .map(|update| update.message.as_str())
            .collect::<Vec<_>>(),
        ["fmt: success", "lint: success", "test: success"]
    );
    assert!(
        updates
            .iter()
            .all(|update| update.delivery == updates[0].delivery)
    );
}

#[tokio::test]
async fn passes_held_past_one_response_are_delivered_in_parts() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "many-checks".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    let names = (0..40)
        .map(|n| format!("{n:02}{}", "x".repeat(2000)))
        .collect::<Vec<_>>();
    // Each poll passes ten more checks while the last stays pending.
    for passed in [0, 10, 20, 30, 39, 40] {
        let checks = names
            .iter()
            .enumerate()
            .map(|(n, name)| {
                (
                    name.as_str(),
                    if n < passed { "success" } else { "pending" },
                )
            })
            .collect::<Vec<_>>();
        manager
            .queue_background_updates(
                &workspace.id,
                vec![("pr".into(), checks_at("a", &checks))],
                crate::time::unix_seconds(),
            )
            .await
            .unwrap();
    }
    let mut received = Vec::new();
    while received.len() < names.len() {
        let delivered = manager.agent_updates(&workspace.id).await.unwrap();
        assert!(!delivered.updates.is_empty() && delivered.updates.len() < names.len());
        manager
            .acknowledge_pr_updates(
                &workspace.id,
                delivered
                    .updates
                    .iter()
                    .map(|update| update.delivery.clone())
                    .collect(),
            )
            .await
            .unwrap();
        received.extend(delivered.updates);
    }
    assert_eq!(received.len(), names.len());
}

#[tokio::test]
async fn own_changes_are_not_reported_back_to_the_workspace() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "own".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    let observe = |comments: Value, state: &str| {
        let snapshot = serde_json::from_value(json!({"comments": comments, "checks": {},
                "conflict": null, "state": state, "errors": {}}))
        .unwrap();
        manager.record_pr_activity(&workspace.id, vec![("pr".into(), snapshot)])
    };
    let first = observe(json!({"discussion:1": "theirs"}), "open")
        .await
        .unwrap();
    manager
        .acknowledge_pr_updates(&workspace.id, vec![first[0].delivery.clone()])
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            let change = OwnChange {
                url: "pr",
                authored: vec!["discussion:2".into()],
                state: Some("closed"),
                edited: Edited::default(),
            };
            record_own_change(db, &id, change)
        })
        .await
        .unwrap();
    // A poll that read the item before the agent closed it records its
    // comments, but not its stale state.
    let stale = serde_json::from_value(json!({"comments": {"discussion:1": "theirs"},
            "checks": {}, "conflict": null, "state": "open", "errors": {}}))
    .unwrap();
    let all = Selection::default();
    let report = BranchReport::Conflicts;
    let before =
        manager.record_observed(&workspace.id, vec![("pr".into(), stale)], &all, report, 0);
    assert!(before.await.unwrap().is_empty());
    let own = json!({"discussion:1": "theirs", "discussion:2": "mine"});
    assert!(observe(own, "closed").await.unwrap().is_empty());
    let next = json!({"discussion:1": "theirs", "discussion:2": "mine", "discussion:3": "new"});
    let updates = observe(next, "closed").await.unwrap();
    assert_eq!(
        updates.iter().map(|update| update.kind).collect::<Vec<_>>(),
        [UpdateKind::Comment]
    );
}

#[tokio::test]
async fn edits_are_reported_except_the_workspaces_own() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "edits".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    let snapshot = |base: &str, labels: Value| -> Snapshot {
        serde_json::from_value(json!({"comments": {}, "checks": {}, "conflict": null,
                "state": "open", "errors": {}, "details": {"title": "Fix", "body": "",
                "labels": labels, "assignees": [], "draft": false, "base": base}}))
        .unwrap()
    };
    let all = Selection::default();
    let observe = |snapshot: Snapshot, at: u64| {
        let observation = vec![("pr".into(), snapshot)];
        manager.record_observed(
            &workspace.id,
            observation,
            &all,
            BranchReport::Conflicts,
            at,
        )
    };
    let now = crate::time::unix_seconds();
    assert!(
        observe(snapshot("feature", json!([])), now)
            .await
            .unwrap()
            .is_empty()
    );
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            let change = OwnChange {
                url: "pr",
                authored: Vec::new(),
                state: None,
                edited: Edited::retarget("feature", "main"),
            };
            record_own_change(db, &id, change)
        })
        .await
        .unwrap();
    // A poll that read the PR before Shoal retargeted it does not report
    // the old base, and the one after does not report the new one.
    let stale = observe(snapshot("feature", json!([])), 0);
    assert!(stale.await.unwrap().is_empty());
    let later = crate::time::unix_seconds() + 1;
    assert!(
        observe(snapshot("main", json!([])), later)
            .await
            .unwrap()
            .is_empty()
    );
    let labelled = observe(snapshot("main", json!(["bug"])), later)
        .await
        .unwrap();
    let [edited] = labelled.as_slice() else {
        panic!("{labelled:?}");
    };
    assert_eq!(edited.kind, UpdateKind::Edited);
    assert!(edited.message.contains("bug"), "{}", edited.message);
}

#[tokio::test]
async fn background_activity_does_not_repeat_an_unchanged_baseline_failure() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "failed-activity".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    let failed: Snapshot = serde_json::from_value(serde_json::json!({
        "comments": {},
        "checks": {},
        "conflict": null,
        "state": "open",
        "errors": {"forge": "temporarily unavailable"}
    }))
    .unwrap();

    manager
        .record_background_activity(&workspace.id, vec![("pr".into(), failed.clone())])
        .await
        .unwrap();
    manager
        .record_background_activity(&workspace.id, vec![("pr".into(), failed)])
        .await
        .unwrap();
    assert!(manager
            .workspace_events(None, 100)
            .await
            .unwrap()
            .iter()
            .all(|item| !matches!(item, crate::daemon::events::EventItem::Event(event) if event.details.kind == crate::daemon::events::EventKind::ItemActivity)));
}

#[tokio::test]
async fn polls_stop_looking_up_items_whose_activity_ended() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "ended".into(), None, None, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_issue(space_id,url) VALUES (?1,'issue')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    // Neither URL names a forge, so looking one up fails.
    let unfiltered = Selection::default();
    let poll = || manager.poll_item_activity(&workspace.id, &unfiltered);
    assert!(poll().await.is_err());

    let ended = |state: &str| {
        serde_json::from_value::<Snapshot>(
            json!({"comments": {}, "checks": {}, "conflict": null, "state": state, "errors": {}}),
        )
        .unwrap()
    };
    let updates = manager
        .record_activity(
            &workspace.id,
            vec![
                ("pr".into(), ended("merged")),
                ("issue".into(), ended("closed")),
            ],
            &Selection::default(),
        )
        .await
        .unwrap();
    let deliveries = updates.into_iter().map(|update| update.delivery).collect();
    manager
        .acknowledge_pr_updates(&workspace.id, deliveries)
        .await
        .unwrap();
    // A closed issue may reopen while the assignment goes on.
    assert!(poll().await.is_err());

    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO workspace_completion(workspace_id,record) VALUES (?1,'{}')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(poll().await.unwrap().is_empty());
    manager.poll_linked_item_activity().await.unwrap();
}

#[tokio::test]
async fn messages_waiting_for_work_come_with_the_next_updates() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "quiet".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    manager
        .queue_agent_message_delivered(
            &workspace.id,
            "Memory is low".into(),
            crate::daemon::agent_messages::Delivery::WhileWorkRuns,
        )
        .await
        .unwrap();
    assert!(!manager.updates_waiting(&workspace.id).await.unwrap());
    assert!(!manager.agent_updates_recorded(&workspace.id).await.unwrap());
    manager
        .record_pr_activity(&workspace.id, vec![("pr".into(), snapshot("review"))])
        .await
        .unwrap();
    // A message that wakes the agent would end the wait on its own.
    let updates = manager.wait_prs(&workspace.id, Some(60)).await.unwrap();
    assert_eq!(updates.updates.len(), 1);
    assert_eq!(updates.messages[0].message, "Memory is low");
}

#[tokio::test]
async fn messages_queued_during_a_wait_come_with_its_updates() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None, Default::default())
        .await
        .unwrap();
    register_watch(&manager, &child.id, "child-pr").await;
    let prs = Selection {
        kind: Some(LinkKind::Pr),
        input: None,
    };
    // Holding the child's gate stops the wait's poll, which takes the
    // parent's gate first, after the wait read its messages.
    let held = manager.gates.lock_prs([&child.id]).await;
    let wait = tokio::spawn({
        let manager = manager.clone();
        let id = parent.id.clone();
        async move { manager.wait_items(&id, &prs, None).await }
    });
    tokio::time::timeout(Duration::from_secs(60), async {
        while manager.gates.pr_gates_in_use() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the wait polls");
    manager
        .queue_agent_message_delivered(
            &parent.id,
            "Memory is low".into(),
            crate::daemon::agent_messages::Delivery::WhileWorkRuns,
        )
        .await
        .unwrap();
    drop(held);
    manager
        .queue_background_updates(
            &child.id,
            vec![("child-pr".into(), snapshot("first"))],
            crate::time::unix_seconds(),
        )
        .await
        .unwrap();
    let updates = tokio::time::timeout(Duration::from_secs(60), wait)
        .await
        .expect("the wait ends")
        .unwrap()
        .unwrap();
    assert_eq!(updates.updates.len(), 1);
    assert_eq!(updates.messages[0].message, "Memory is low");
}

#[tokio::test]
async fn recorded_updates_count_while_the_pr_gate_is_held() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "gated".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    assert!(!manager.agent_updates_recorded(&workspace.id).await.unwrap());
    manager
        .record_pr_activity(&workspace.id, vec![("pr".into(), snapshot("review"))])
        .await
        .unwrap();
    // Cleanup holds the gate while it decides whether updates wait.
    let _gate = manager.gates.lock_prs([&workspace.id]).await;
    assert!(!manager.updates_waiting(&workspace.id).await.unwrap());
    assert!(manager.agent_updates_recorded(&workspace.id).await.unwrap());
}

#[tokio::test]
async fn linked_filters_preserve_other_cursors_and_unlink_drops_inflight_activity() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "items".into(), None, None, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_issue(space_id,url) VALUES (?1,'issue')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    let updates = manager
        .record_activity(
            &workspace.id,
            vec![
                ("pr".into(), snapshot("review")),
                ("issue".into(), snapshot("comment")),
            ],
            &Selection::default(),
        )
        .await
        .unwrap();
    assert_eq!(updates.len(), 2);
    let pending = manager
        .pending_activity(&workspace.id, &["pr".into()])
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    manager
        .acknowledge_pr_updates(&workspace.id, vec![pending[0].delivery.clone()])
        .await
        .unwrap();
    manager
        .set_pr(&workspace.id, super::super::pr::Action::Clear)
        .await
        .unwrap();
    assert_eq!(
        manager
            .pending_activity(&workspace.id, &["issue".into()])
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        manager
            .record_activity(
                &workspace.id,
                vec![("pr".into(), snapshot("later"))],
                &Selection::default()
            )
            .await
            .unwrap()
            .is_empty()
    );
    manager.clear_issue(&workspace.id, None).await.unwrap();
    assert!(
        manager
            .pending_activity(&workspace.id, &["issue".into()])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        manager
            .record_activity(
                &workspace.id,
                vec![("issue".into(), snapshot("later"))],
                &Selection::default()
            )
            .await
            .unwrap()
            .is_empty()
    );
    let explicit = Selection {
        kind: Some(LinkKind::Issue),
        input: Some("2".into()),
    };
    let updates = manager
        .record_activity(
            &workspace.id,
            vec![("explicit".into(), snapshot("new"))],
            &explicit,
        )
        .await
        .unwrap();
    assert_eq!(updates.len(), 1);
    assert!(
        manager
            .issue_registration(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        manager
            .pr_registration(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn blocked_gate_and_remote_poll_respect_the_timeout() {
    let gate = tokio::sync::Mutex::new(());
    let held = gate.lock().await;
    let started = tokio::time::Instant::now();
    let result = wait_for_updates(
        watch_deadline(Some(1)),
        &mut tokio::sync::watch::channel(0).1,
        || async {
            let _guard = gate.lock().await;
            Ok(Vec::new())
        },
    )
    .await
    .unwrap();
    assert!(result.timed_out);
    assert!(result.updates.is_empty());
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(1)
    );
    drop(held);
    let started = tokio::time::Instant::now();
    let result = wait_for_updates(
        watch_deadline(Some(1)),
        &mut tokio::sync::watch::channel(0).1,
        std::future::pending,
    )
    .await
    .unwrap();
    assert!(result.timed_out);
    assert!(result.updates.is_empty());
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(1)
    );
}

#[tokio::test]
async fn acknowledgement_keeps_queued_updates_and_those_a_newer_wait_needs() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "queued".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    let delivered = manager
        .record_pr_activity(&workspace.id, vec![("pr".into(), snapshot("review"))])
        .await
        .unwrap();
    let queued = Update {
        url: "pr".into(),
        kind: UpdateKind::BaseMerged,
        message: "base merged".into(),
        delivery: "queued".into(),
        check: None,
        workspace: None,
    };
    let (id, update) = (workspace.id.clone(), queued.clone());
    manager
        .store
        .run(move |db| queue_update(db, &id, update))
        .await
        .unwrap();
    let newer = manager.item_waits.start(&workspace.id);
    manager
        .acknowledge_pr_updates(&workspace.id, vec![delivered[0].delivery.clone()])
        .await
        .unwrap();
    assert_eq!(
        manager.pending_pr_activity(&workspace.id).await.unwrap(),
        [delivered[0].clone(), queued.clone()]
    );
    drop(newer);
    manager
        .acknowledge_pr_updates(&workspace.id, vec![delivered[0].delivery.clone()])
        .await
        .unwrap();
    assert_eq!(
        manager.pending_pr_activity(&workspace.id).await.unwrap(),
        [queued]
    );
}

#[tokio::test]
async fn cursors_survive_restart_are_workspace_owned_and_clear_on_unwatch() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "watch".into(), None, None, None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let changes = || vec![("pr".into(), snapshot("first review"))];
    register_watch(&manager, &workspace.id, "pr").await;
    register_watch(&manager, &other.id, "pr").await;
    let first = manager
        .record_pr_activity(&workspace.id, changes())
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    let replay = manager
        .record_pr_activity(&workspace.id, changes())
        .await
        .unwrap();
    assert_eq!(replay, first);
    assert_eq!(
        manager
            .record_pr_activity(&other.id, changes())
            .await
            .unwrap()
            .len(),
        1
    );
    let restarted = Manager::open(manager.paths.clone()).await.unwrap();
    assert_eq!(
        restarted.pending_pr_activity(&workspace.id).await.unwrap(),
        first
    );
    restarted
        .acknowledge_pr_updates(&workspace.id, vec!["stale-delivery".into()])
        .await
        .unwrap();
    assert_eq!(
        restarted.pending_pr_activity(&workspace.id).await.unwrap(),
        first
    );
    restarted
        .acknowledge_pr_updates(&workspace.id, vec![first[0].delivery.clone()])
        .await
        .unwrap();
    assert!(
        restarted
            .record_pr_activity(&workspace.id, changes())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        restarted
            .record_pr_activity(
                &workspace.id,
                vec![("pr".into(), snapshot("edited review"))]
            )
            .await
            .unwrap()
            .len(),
        1
    );
    restarted
        .set_pr(&workspace.id, super::super::pr::Action::Clear)
        .await
        .unwrap();
    let id = workspace.id.clone();
    assert_eq!(
        restarted
            .store
            .run(move |db| Ok(db.query_row(
                "SELECT COUNT(*) FROM pr_activity WHERE space_id=?1",
                [id],
                |row| row.get::<_, i64>(0)
            )?))
            .await
            .unwrap(),
        0
    );
    assert!(
        restarted
            .record_pr_activity(&workspace.id, changes())
            .await
            .is_err()
    );
    register_watch(&restarted, &workspace.id, "another-pr").await;
    assert!(
        restarted
            .record_pr_activity(&workspace.id, changes())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        restarted
            .pending_pr_activity(&workspace.id)
            .await
            .unwrap()
            .is_empty()
    );
    restarted
        .set_pr(&workspace.id, super::super::pr::Action::Clear)
        .await
        .unwrap();
    assert!(restarted.wait_prs(&workspace.id, Some(0)).await.is_err());
    assert!(restarted.wait_prs(&workspace.id, Some(3601)).await.is_err());
    assert!(restarted.wait_prs(&workspace.id, Some(3600)).await.is_err());
    assert!(restarted.completion(&workspace.id).await.unwrap().is_none());
}

#[tokio::test]
async fn unfiltered_waits_without_a_pr_report_new_branch_conflicts() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "branch".into(), None, None, None)
        .await
        .unwrap();
    let all = Selection::default();

    // A linked PR's forge reports conflicts instead.
    register_watch(&manager, &workspace.id, "pr").await;
    let conflicting = branch_conflicts(
        Ok(crate::model::ConflictCheck {
            workspace_id: workspace.id.clone(),
            target: "main".into(),
            target_commit: String::new(),
            head: String::new(),
            conflicts: true,
            files: vec!["tracked".into()],
            changed_on_both_sides: Vec::new(),
        }),
        BranchReport::Conflicts,
    );
    let observation = vec![(workspace.branch.clone().unwrap(), conflicting)];
    let recorded = manager.record_activity(&workspace.id, observation, &all);
    assert!(recorded.await.unwrap().is_empty());
    manager
        .set_pr(&workspace.id, super::super::pr::Action::Clear)
        .await
        .unwrap();

    let poll = || manager.poll_item_activity(&workspace.id, &all);
    assert!(poll().await.unwrap().is_empty());
    for (repo, content) in [(&workspace.path, "workspace\n"), (&repo_path, "main\n")] {
        std::fs::write(repo.join("tracked"), content).unwrap();
        crate::test_support::commit(repo, "tracked");
    }
    let updates = poll().await.unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(Some(&updates[0].url), workspace.branch.as_ref());
    assert_eq!(updates[0].kind, UpdateKind::MergeConflict);
    assert!(
        updates[0].message.ends_with(": tracked"),
        "{}",
        updates[0].message
    );
    assert_eq!(
        poll().await.unwrap(),
        updates,
        "unacknowledged updates replay"
    );
    manager
        .acknowledge_pr_updates(&workspace.id, vec![updates[0].delivery.clone()])
        .await
        .unwrap();
    assert!(
        poll().await.unwrap().is_empty(),
        "a persisting conflict is not new"
    );
    let issues = Selection {
        kind: Some(LinkKind::Issue),
        input: None,
    };
    let polled = manager.poll_item_activity(&workspace.id, &issues).await;
    assert!(polled.is_err());

    // A failing issue lookup still lets a new branch conflict through.
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_issue(space_id,url) VALUES (?1,'issue')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(poll().await.is_err());
    std::fs::write(repo_path.join("other"), "main\n").unwrap();
    crate::test_support::commit(&repo_path, "other");
    std::fs::write(workspace.path.join("other"), "workspace\n").unwrap();
    crate::test_support::commit(&workspace.path, "other");
    let previous = manager.record_activity(
        &workspace.id,
        vec![(
            workspace.branch.clone().unwrap(),
            resolved_branch(&workspace.id),
        )],
        &all,
    );
    assert!(previous.await.unwrap().is_empty());
    let updates = poll().await.unwrap();
    assert_eq!(updates.len(), 1);
    let message = &updates[0].message;
    assert!(message.ends_with(": other, tracked"), "{message}");
}

#[tokio::test]
async fn sensitive_files_changed_on_both_sides_are_reported_beside_a_linked_pr() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    std::fs::write(
        repo_path.join(".shoal.toml"),
        "[conflicts]\nsensitive = ['version']\n",
    )
    .unwrap();
    crate::test_support::commit(&repo_path, ".shoal.toml");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut workspaces = Vec::new();
    for name in ["solo", "linked"] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        for (file, content) in [("version", "2\n"), ("tracked", "workspace\n")] {
            std::fs::write(workspace.path.join(file), content).unwrap();
        }
        crate::test_support::commit(&workspace.path, ".");
        workspaces.push(workspace);
    }
    for (file, content) in [("version", "2\n"), ("tracked", "main\n")] {
        std::fs::write(repo_path.join(file), content).unwrap();
    }
    crate::test_support::commit(&repo_path, ".");
    let all = Selection::default();

    let updates = manager.poll_item_activity(&workspaces[0].id, &all).await;
    let updates = updates.unwrap();
    assert_eq!(updates.len(), 1);
    let message = &updates[0].message;
    assert!(
        message.contains("tracked") && message.contains("version"),
        "{message}"
    );

    // The forge reports the PR's conflicts, so the branch reports the rest,
    // from a watch and from the background poll.
    let linked = &workspaces[1];
    register_watch(&manager, &linked.id, "pr").await;
    let space = manager.link_space(&linked.id).await.unwrap();
    let polled = manager.poll_space_item_activity(&space).await;
    assert!(polled.is_err(), "the PR lookup fails");
    let queued = manager.queued_updates(linked.clone()).await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(Some(&queued[0].url), linked.branch.as_ref());
    assert_eq!(queued[0].kind, UpdateKind::MergeConflict);
    let message = &queued[0].message;
    assert!(
        message.contains("version") && !message.contains("tracked"),
        "{message}"
    );
    let updates = manager.poll_item_activity(&linked.id, &all).await;
    assert_eq!(updates.unwrap(), queued, "the watch reports it once");
    let recorded = manager.recorded_changed_on_both_sides(&linked.id).await;
    assert_eq!(recorded.unwrap().unwrap().files, ["version"]);
}

#[tokio::test]
async fn a_stalled_worktree_fails_the_background_branch_check_at_once() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "stalled".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    tokio::time::pause();
    let stalled = manager
        .check_access(&workspace.path, std::future::pending::<Result<()>>())
        .await;
    tokio::time::resume();
    assert!(stalled.is_err());

    // Even reading which files the repository lists waits for the worktree.
    let space = manager.link_space(&workspace.id).await.unwrap();
    let polled = manager.poll_space_item_activity(&space).await;
    assert!(polled.is_err(), "the PR lookup fails");
    let queued = manager.queued_updates(workspace.clone()).await.unwrap();
    let stall = queued
        .iter()
        .find(|update| Some(&update.url) == workspace.branch.as_ref());
    let stall = stall.unwrap_or_else(|| panic!("no branch update in {queued:?}"));
    assert_eq!(stall.kind, UpdateKind::LookupFailed);
}

#[tokio::test]
async fn branch_conflicts_with_a_target_that_has_heads_changes_are_not_reported() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "branch".into(), None, None, None)
        .await
        .unwrap();
    std::fs::write(workspace.path.join("tracked"), "workspace\n").unwrap();
    crate::test_support::commit(&workspace.path, "tracked");
    // The default branch takes the commit under a new SHA, then edits it.
    std::fs::write(repo_path.join("other"), "main\n").unwrap();
    crate::test_support::commit(&repo_path, "other");
    crate::test_support::git(
        &repo_path,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "cherry-pick",
            "branch",
        ],
    );
    std::fs::write(repo_path.join("tracked"), "main\n").unwrap();
    crate::test_support::commit(&repo_path, "tracked");
    assert!(
        manager
            .check_conflicts(&workspace.id, None)
            .await
            .unwrap()
            .conflicts
    );
    let all = Selection::default();
    assert!(
        manager
            .poll_item_activity(&workspace.id, &all)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn branch_conflicts_of_work_integrated_elsewhere_are_not_reported() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "branch".into(), None, None, None)
        .await
        .unwrap();
    manager
        .write_local_config(&repo.id, Some("[cleanup.done]\nenabled = false\n".into()))
        .await
        .unwrap();
    for (repo, content) in [(&workspace.path, "workspace\n"), (&repo_path, "main\n")] {
        std::fs::write(repo.join("tracked"), content).unwrap();
        crate::test_support::commit(repo, "tracked");
    }
    manager
        .mark_done(&workspace.id, false, true, vec![], &Attacher::User)
        .await
        .unwrap();
    let all = Selection::default();
    let poll = || manager.poll_item_activity(&workspace.id, &all);
    assert!(poll().await.unwrap().is_empty());
    // Withdrawing the completion reports the conflict again.
    manager.undo_done(&workspace.id).await.unwrap();
    let updates = poll().await.unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].kind, UpdateKind::MergeConflict);
}

fn resolved_branch(workspace_id: &str) -> Snapshot {
    branch_conflicts(
        Ok(crate::model::ConflictCheck {
            workspace_id: workspace_id.into(),
            target: "main".into(),
            target_commit: String::new(),
            head: String::new(),
            conflicts: false,
            files: Vec::new(),
            changed_on_both_sides: Vec::new(),
        }),
        BranchReport::Conflicts,
    )
}

/// A run in `state`, as a poll observes it.
fn run_snapshot(state: &str) -> Snapshot {
    serde_json::from_value(json!({"comments": {}, "checks": {}, "conflict": null,
        "state": state, "errors": {}}))
    .unwrap()
}

#[tokio::test]
async fn background_activity_records_a_run_finish_a_watch_delivered_first() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "runs".into(), None, None, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    let run = crate::model::LinkedItem {
        kind: LinkKind::Ci,
        url: "run".into(),
        title: None,
    };
    manager
        .record_items(&id, vec![(crate::model::Relation::Related, run)])
        .await
        .unwrap();
    manager
        .record_background_activity(&id, vec![("run".into(), run_snapshot("running"))])
        .await
        .unwrap();

    // A watch sees the run finish and delivers it before the background poll.
    let selection = Selection {
        kind: Some(LinkKind::Ci),
        input: None,
    };
    let delivered = manager
        .record_activity(
            &id,
            vec![("run".into(), run_snapshot("finished"))],
            &selection,
        )
        .await
        .unwrap();
    assert_eq!(delivered.len(), 1, "{delivered:?}");
    manager
        .acknowledge_pr_updates(&id, vec![delivered[0].delivery.clone()])
        .await
        .unwrap();
    let (watched, polled) = manager
        .store
        .run({
            let id = id.clone();
            move |db| Ok((followed_items(db, &id, None)?, polled_items(db, &id)?))
        })
        .await
        .unwrap();
    assert!(watched.is_empty(), "{watched:?}");
    assert_eq!(polled, [("run".to_owned(), LinkKind::Ci)]);

    // The background poll still records the finish on its own baseline.
    manager
        .record_background_activity(&id, vec![("run".into(), run_snapshot("finished"))])
        .await
        .unwrap();
    let events = manager.workspace_events(None, 100).await.unwrap();
    assert!(events.iter().any(|item| matches!(item,
        crate::daemon::events::EventItem::Event(event)
            if event.details.item_activity.as_deref().is_some_and(|activity|
                activity.url == "run" && activity.update.kind == UpdateKind::CiCompleted))));
}

#[tokio::test]
async fn background_polls_a_finished_run_until_its_finish_is_queued() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "runs".into(), None, None, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    let run = crate::model::LinkedItem {
        kind: LinkKind::Ci,
        url: "run".into(),
        title: None,
    };
    manager
        .record_items(&id, vec![(crate::model::Relation::Related, run)])
        .await
        .unwrap();
    let polled = || {
        let id = id.clone();
        manager.store.run(move |db| polled_items(db, &id))
    };
    // The poll is interrupted after recording the finish on its baseline.
    let finished = vec![("run".to_owned(), run_snapshot("finished"))];
    manager
        .record_background_activity(&id, finished.clone())
        .await
        .unwrap();
    assert_eq!(polled().await.unwrap(), [("run".to_owned(), LinkKind::Ci)]);
    // The next poll queues it for the agent's hooks, which ends the polling.
    manager
        .queue_background_updates(&id, finished, crate::time::unix_seconds())
        .await
        .unwrap();
    assert!(polled().await.unwrap().is_empty());
    let queued = manager.agent_updates(&id).await.unwrap().updates;
    assert_eq!(queued[0].kind, UpdateKind::CiCompleted, "{queued:?}");
}

/// A workspace whose repository holds agent messages for a minute.
async fn settling_workspace(root: &tempfile::TempDir, manager: &Manager) -> Workspace {
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .write_local_config(&repo.id, Some("[messages]\nsettle_seconds = 60\n".into()))
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "settling".into(), None, None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn updates_leave_settling_messages_for_a_later_delivery() {
    let (root, manager) = manager().await;
    let workspace = settling_workspace(&root, &manager).await;
    register_watch(&manager, &workspace.id, "pr").await;
    manager
        .queue_agent_message_delivered(
            &workspace.id,
            "Memory is low".into(),
            crate::daemon::agent_messages::Delivery::WhileWorkRuns,
        )
        .await
        .unwrap();
    manager
        .record_pr_activity(&workspace.id, vec![("pr".into(), snapshot("review"))])
        .await
        .unwrap();
    let updates = manager.wait_prs(&workspace.id, Some(60)).await.unwrap();
    assert_eq!(updates.updates.len(), 1);
    assert!(updates.messages.is_empty(), "{updates:?}");
    let unread = manager.agent_messages(&workspace.id).await.unwrap();
    assert_eq!(unread.len(), 1);
}

#[tokio::test]
async fn a_watch_times_out_while_its_messages_settle() {
    let (root, manager) = manager().await;
    let workspace = settling_workspace(&root, &manager).await;
    manager
        .send_agent_message(
            &workspace.id,
            "Request 1 approved".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let updates = manager
        .wait_items(&workspace.id, &Selection::default(), Some(1))
        .await
        .unwrap();
    assert!(
        updates.timed_out && updates.messages.is_empty(),
        "{updates:?}"
    );
    let unread = manager.agent_messages(&workspace.id).await.unwrap();
    assert_eq!(unread.len(), 1);
}

#[test]
fn settling_messages_delay_a_wake_but_keep_an_issued_one_due() {
    let settling = Some(Duration::from_secs(5));
    assert!(!wake_messages_due(true, settling, true));
    assert!(wake_messages_due(true, None, true));
    // A message arriving after the prompt does not make the agent look
    // finished with the ones it was prompted for.
    assert!(wake_messages_due(true, settling, false));
    assert!(!wake_messages_due(false, None, true));
    assert!(!wake_messages_due(false, None, false));
}

#[tokio::test]
async fn a_held_gate_delays_a_wake_but_keeps_an_issued_one_due() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "gated".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    manager
        .record_pr_activity(&workspace.id, vec![("pr".into(), snapshot("review"))])
        .await
        .unwrap();
    let held = manager.gates.lock_prs([&workspace.id]).await;
    assert!(
        !manager
            .wake_updates_due(workspace.clone(), true)
            .await
            .unwrap()
    );
    // Cleanup taking the gate after the prompt does not make the agent
    // look finished with the updates it was prompted for.
    assert!(
        manager
            .wake_updates_due(workspace.clone(), false)
            .await
            .unwrap()
    );
    drop(held);
    assert!(manager.wake_updates_due(workspace, true).await.unwrap());
}

#[tokio::test]
async fn a_merge_a_background_poll_sees_asks_for_a_sync() {
    let (root, manager) = manager().await;
    let repo_path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo_path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "merging".into(), None, None, None)
        .await
        .unwrap();
    register_watch(&manager, &workspace.id, "pr").await;
    let in_state = |state: &str| {
        serde_json::from_value::<Snapshot>(
            json!({"comments": {}, "checks": {}, "conflict": null, "state": state, "errors": {}}),
        )
        .unwrap()
    };
    let record = |state| {
        manager.record_background_activity(&workspace.id, vec![("pr".into(), in_state(state))])
    };
    assert!(!record("open").await.unwrap());
    assert!(!record("open").await.unwrap());
    assert!(record("merged").await.unwrap());
}

#[tokio::test]
async fn the_background_poll_reads_spaces_together() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // Each workspace's issue is answered only once both are being read.
    let arrived = crate::forge::test_support::together(2);
    let server = crate::forge::test_support::Server::start(move |request| {
        let Some(number) = request
            .line
            .strip_prefix("GET /api/v1/repos/team/repo/issues/")
        else {
            return (404, Vec::new(), Vec::new());
        };
        if number.contains("/comments") {
            return (200, Vec::new(), "[]".into());
        }
        if !arrived() {
            return (500, Vec::new(), Vec::new());
        }
        let issue = json!({"number": number.parse::<u64>().unwrap(), "title": "Fix",
            "state": "open"});
        (200, Vec::new(), issue.to_string().into())
    });
    let host = &server.host;
    // Pins are this process's, which parallel tests replace; an answer stays.
    crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::Forgejo);
    crate::test_support::configure(
        &manager,
        &format!("[machine.forges.\"{host}\"]\ntype = \"forgejo\"\ntoken = \"user\"\n"),
    )
    .await;
    let mut workspaces = Vec::new();
    for number in [1, 2] {
        let name = format!("issue-{number}");
        let created = manager.create_workspace(&repo.id, name, None, None, None);
        workspaces.push((number, created.await.unwrap()));
    }
    let origin = format!("http://{host}/team/repo");
    crate::test_support::git(&path, &["remote", "add", "origin", &origin]);
    for (number, workspace) in workspaces {
        let issue = format!("http://{host}/team/repo/issues/{number}");
        manager
            .set_issue(&workspace.id, &issue, Some("Fix".into()))
            .await
            .unwrap();
    }
    manager.poll_linked_item_activity().await.unwrap();
}
