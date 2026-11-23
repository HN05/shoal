use crate::daemon::events::*;
use crate::model::EventCause;
use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        notifications::AlertKind,
        test_support::{git, manager, repository},
        workspace_files::Attacher,
    },
    protocol::{self, Body, Response, SpaceMethod},
};
use anyhow::Result;
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::BranchName;
use std::{sync::Arc, time::Duration};
use tokio::{io::BufReader, net::UnixStream, task::JoinHandle};

async fn receive(stream: &mut BufReader<UnixStream>) -> Body {
    tokio::time::timeout(Duration::from_secs(60), async {
        protocol::read_buffered::<Response>(stream)
            .await
            .unwrap()
            .body
    })
    .await
    .expect("event stream hung")
}

async fn connect(
    manager: Arc<Manager>,
    after: Option<i64>,
    follow: bool,
) -> (BufReader<UnixStream>, JoinHandle<Result<()>>) {
    let (server, client) = UnixStream::pair().unwrap();
    let mut client = BufReader::new(client);
    let startup = manager.gates.owned_operation().await;
    let task = tokio::spawn(crate::daemon::requests::spaces::watch_space_events(
        server,
        1,
        manager,
        after.map(EventCursor::new),
        follow,
        startup,
    ));
    assert!(matches!(receive(&mut client).await, Body::Ok));
    (client, task)
}

async fn seed(manager: &Manager, count: usize) {
    journal(manager, count, r#"{"kind":"ready","space_id":"w","space_kind":"workspace","repository_id":"r","name":"worker","path":"/work","branch":"worker","cause":null,"error":null}"#).await;
}

/// Journal `count` turn states of one workspace, which keeps its newest 500.
async fn seed_turns(manager: &Manager, count: usize) {
    journal(manager, count, r#"{"kind":"agent_state","space_id":"w","space_kind":"workspace","repository_id":"r","name":"worker","path":"/work","branch":"worker","cause":null,"error":null,"agent_state":{"state":"idle"}}"#).await;
}

async fn journal(manager: &Manager, count: usize, record: &'static str) {
    manager
        .store
        .write(move |tx| {
            for _ in 0..count {
                tx.execute(
                    "INSERT INTO space_events(space_id,record)
                     VALUES (json_extract(?1,'$.space_id'),?1)",
                    [record],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn replay_drains_multiple_batches_and_live_readers_do_not_consume_notifications() {
    let (_temp, manager) = manager().await;
    seed(&manager, 205).await;
    manager
        .coordination
        .notify(
            &*manager,
            Some(&"worker".into()),
            AlertKind::SpaceRemoved,
            "idle cleanup",
        )
        .await;
    let (mut first, first_task) = connect(manager.clone(), Some(0), true).await;
    let (mut second, second_task) = connect(manager.clone(), Some(200), true).await;
    for id in 1..=205 {
        let Body::EventItem(EventItem::Event(event)) = receive(&mut first).await else {
            panic!("expected event");
        };
        assert_eq!(event.id.id(), id);
    }
    for id in 201..=205 {
        let Body::EventItem(EventItem::Event(event)) = receive(&mut second).await else {
            panic!("expected event");
        };
        assert_eq!(event.id.id(), id);
    }
    seed(&manager, 1).await;
    for stream in [&mut first, &mut second] {
        let Body::EventItem(EventItem::Event(event)) = receive(stream).await else {
            panic!("expected live event");
        };
        assert_eq!(event.id.id(), 206);
    }
    assert_eq!(manager.notifications.unread().await.unwrap(), 1);
    drop(first);
    drop(second);
    first_task.await.unwrap().unwrap();
    second_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn finite_replay_reports_gaps_and_survives_reopen() {
    let (_temp, manager) = manager().await;
    seed_turns(&manager, 1005).await;
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    let (mut stream, task) = connect(reopened, Some(0), false).await;
    let Body::EventItem(EventItem::Gap {
        oldest_id,
        latest_id,
        ..
    }) = receive(&mut stream).await
    else {
        panic!("expected gap");
    };
    let (oldest_id, latest_id) = (oldest_id.id(), latest_id.id());
    assert_eq!((oldest_id, latest_id), (506, 1005));
    let count = latest_id - oldest_id + 1;
    for index in 0..count {
        let Body::EventItem(EventItem::Event(event)) = receive(&mut stream).await else {
            panic!("expected retained event");
        };
        assert_eq!(event.id.id(), oldest_id + index);
    }
    assert!(matches!(receive(&mut stream).await, Body::Ok));
    task.await.unwrap().unwrap();
    let (mut empty, task) = connect(manager, Some(latest_id), false).await;
    assert!(matches!(receive(&mut empty).await, Body::Ok));
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn branch_observations_preserve_ownership_and_scope_denies_events() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    for follow in [false, true] {
        let mut method = SpaceMethod::WatchSpaceEvents {
            after: Some(EventCursor::new(0)),
            follow,
        };
        assert!(
            crate::daemon::scope::authorize(
                &manager,
                Some(&execution.plan.scope_token),
                &mut method
            )
            .await
            .is_err()
        );
        assert!(
            crate::daemon::scope::authorize(&manager, None, &mut method)
                .await
                .is_ok()
        );
    }
    git(&workspace.path, &["checkout", "-b", "other"]);
    manager
        .spaces
        .observe_workspace_branch(&*manager, &workspace)
        .await
        .unwrap();
    git(&workspace.path, &["checkout", "--detach"]);
    manager
        .spaces
        .observe_workspace_branch(&*manager, &workspace)
        .await
        .unwrap();
    assert_eq!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .branch()
            .map(BranchName::as_str),
        Some("worker")
    );
    let events = manager.space_events(None, 100).await.unwrap();
    let changed: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EventItem::Event(e) if e.details.kind == EventKind::BranchChanged => {
                Some(e.details.space.branch().map(BranchName::as_str))
            }
            _ => None,
        })
        .collect();
    assert_eq!(changed, [Some("other"), None]);
}

#[tokio::test]
async fn removal_causes_and_retained_errors_survive_ownership_and_notification_reads() {
    use crate::{
        daemon::links::pr::Action,
        removal::{InspectionPolicy, UnsavedWork},
    };
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    for (name, cause) in [
        ("manual", EventCause::Manual),
        ("idle", EventCause::Idle),
        ("merged", EventCause::Merged),
        ("pr", EventCause::Pr),
        ("missing", EventCause::MissingDirectory),
    ] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        match cause {
            EventCause::Manual => {
                manager
                    .spaces
                    .remove_selected(
                        &*manager,
                        &SpaceSelector::id(&workspace.id),
                        UnsavedWork::default(),
                        InspectionPolicy::GitOnly,
                    )
                    .await
                    .unwrap();
            }
            EventCause::Idle => {
                let snapshot = manager
                    .spaces
                    .cleanup_snapshot(&*manager, &workspace.id)
                    .await
                    .unwrap()
                    .unwrap();
                manager
                    .spaces
                    .remove_idle(&*manager, &SpaceSelector::id(&workspace.id), snapshot)
                    .await
                    .unwrap();
            }
            EventCause::Merged => {
                let snapshot = manager
                    .spaces
                    .merged_snapshot(&*manager, &workspace.id)
                    .await
                    .unwrap()
                    .unwrap();
                manager
                    .spaces
                    .remove_merged_idle(&*manager, &SpaceSelector::id(&workspace.id), snapshot)
                    .await
                    .unwrap();
            }
            EventCause::Pr => {
                manager
                    .links
                    .set_pr(
                        &*manager,
                        &SpaceSelector::id(&workspace.id),
                        Action::Acknowledge,
                    )
                    .await
                    .unwrap();
                manager.sweep_prs().await.unwrap();
            }
            EventCause::MissingDirectory => {
                std::fs::remove_dir_all(&workspace.path).unwrap();
                crate::daemon::cleanup::sweep(&manager, &mut Default::default())
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
        let events = manager.space_events(None, 100).await.unwrap();
        assert!(events.iter().any(|e| matches!(e, EventItem::Event(e) if e.details.space_id == workspace.id && e.details.kind == EventKind::Removed && e.details.cause == Some(cause))));
    }
    let workspace = manager
        .create_workspace(&repo.id, "retain".into(), None, None, None)
        .await
        .unwrap();
    std::fs::write(workspace.path.join("dirty"), "keep this work").unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    let notifications = manager.notifications.list(true, 100).await.unwrap();
    manager
        .notifications
        .mark_read(notifications.iter().map(|n| n.id).collect())
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    let events = manager.space_events(None, 100).await.unwrap();
    let errors: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EventItem::Event(e)
                if e.details.space_id == workspace.id && e.details.kind == EventKind::Retained =>
            {
                Some(e.details.error.as_deref().unwrap())
            }
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(
        std::fs::read_to_string(workspace.path.join("dirty")).unwrap(),
        "keep this work"
    );
}

#[tokio::test]
async fn boundary_future_and_empty_cursors_have_explicit_gap_semantics() {
    let (_temp, manager) = manager().await;
    let (mut stream, task) = connect(manager.clone(), Some(i64::MAX), true).await;
    assert!(matches!(
        receive(&mut stream).await,
        Body::EventItem(EventItem::Gap {
            oldest_id,
            latest_id,
            ..
        }) if (oldest_id.id(), latest_id.id()) == (1, 0)
    ));
    seed_turns(&manager, 1).await;
    assert!(matches!(
        receive(&mut stream).await,
        Body::EventItem(EventItem::Event(event)) if event.id.id() == 1
    ));
    drop(stream);
    task.await.unwrap().unwrap();
    seed_turns(&manager, 1004).await;
    let boundary = manager
        .space_events(Some(EventCursor::new(505)), 1000)
        .await
        .unwrap();
    assert_eq!(boundary.len(), 500);
    assert!(matches!(
        &boundary[0],
        EventItem::Event(event) if event.id.id() == 506
    ));
    let future = manager
        .space_events(Some(EventCursor::new(1006)), 1000)
        .await
        .unwrap();
    assert!(matches!(
        future[0],
        EventItem::Gap {
            after,
            oldest_id,
            latest_id,
        } if (after.id(), oldest_id.id(), latest_id.id()) == (1006, 506, 1005)
    ));
    assert_eq!(future.len(), 501);
    assert!(
        manager
            .space_events(Some(EventCursor::new(-1)), 100)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_cursor_below_what_retention_pruned_reports_the_gap() {
    let (_temp, manager) = manager().await;
    seed(&manager, 1).await;
    // Ids 2 to 601; the space keeps the newest 500, so 2 to 101 go.
    seed_turns(&manager, 600).await;
    seed(&manager, 1).await;
    let behind = manager
        .space_events(Some(EventCursor::new(50)), 1000)
        .await
        .unwrap();
    assert!(matches!(
        behind[0],
        EventItem::Gap {
            after,
            oldest_id,
            latest_id,
        } if (after.id(), oldest_id.id(), latest_id.id()) == (50, 51, 602)
    ));
    assert!(matches!(&behind[1], EventItem::Event(event) if event.id.id() == 102));
    let past = manager
        .space_events(Some(EventCursor::new(101)), 1000)
        .await
        .unwrap();
    assert!(matches!(&past[0], EventItem::Event(event) if event.id.id() == 102));
    let all = manager.space_events(None, 1000).await.unwrap();
    assert!(matches!(&all[0], EventItem::Event(event) if event.id.id() == 1));
}

#[tokio::test]
async fn a_paged_full_replay_keeps_every_event_retention_left() {
    let (_temp, manager) = manager().await;
    seed(&manager, 1).await;
    // Another space's turns, of which retention deletes ids 2 to 101.
    seed_turns(&manager, 600).await;
    seed(&manager, 1).await;
    let (mut stream, task) = connect(manager, None, false).await;
    let mut ids = Vec::new();
    loop {
        match receive(&mut stream).await {
            Body::EventItem(EventItem::Event(event)) => ids.push(event.id.id()),
            Body::Ok => break,
            body => panic!("expected only events: {body:?}"),
        }
    }
    task.await.unwrap().unwrap();
    assert_eq!(ids.len(), 502);
    assert_eq!((ids[0], ids[1], ids[501]), (1, 102, 602));
}
