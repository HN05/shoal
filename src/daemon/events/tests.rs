use super::*;
use crate::{
    daemon::{notifications::NotificationKind, workspace::ExecutionKind},
    protocol::{self, Body, Method, Response},
    test_support::{git, manager, repository},
};
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
    since: Option<i64>,
    follow: bool,
) -> (BufReader<UnixStream>, JoinHandle<Result<()>>) {
    let (server, client) = UnixStream::pair().unwrap();
    let mut client = BufReader::new(client);
    let startup = manager.background_operations.clone().read_owned().await;
    let task = tokio::spawn(crate::daemon::watch_workspace_events(
        server, 1, manager, since, follow, startup,
    ));
    assert!(matches!(receive(&mut client).await, Body::Ok));
    (client, task)
}

async fn seed(manager: &Manager, count: usize) {
    manager.store.run(move |db| {
        let tx = db.transaction()?;
        for _ in 0..count {
            tx.execute("INSERT INTO workspace_events(record) VALUES (?1)", [r#"{"kind":"ready","workspace_id":"w","repository_id":"r","name":"worker","path":"/work","branch":"worker","cause":null,"error":null}"#])?;
        }
        tx.commit()?;
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn replay_drains_multiple_batches_and_live_readers_do_not_consume_notifications() {
    let (_temp, manager) = manager().await;
    seed(&manager, 205).await;
    manager
        .notify(
            Some("worker"),
            NotificationKind::WorkspaceRemoved,
            "idle cleanup",
        )
        .await;
    let (mut first, first_task) = connect(manager.clone(), Some(0), true).await;
    let (mut second, second_task) = connect(manager.clone(), Some(200), true).await;
    for id in 1..=205 {
        let Body::EventItem(EventItem::Event(event)) = receive(&mut first).await else {
            panic!("expected event");
        };
        assert_eq!(event.id, id);
    }
    for id in 201..=205 {
        let Body::EventItem(EventItem::Event(event)) = receive(&mut second).await else {
            panic!("expected event");
        };
        assert_eq!(event.id, id);
    }
    seed(&manager, 1).await;
    for stream in [&mut first, &mut second] {
        let Body::EventItem(EventItem::Event(event)) = receive(stream).await else {
            panic!("expected live event");
        };
        assert_eq!(event.id, 206);
    }
    assert_eq!(manager.unread_notifications().await.unwrap(), 1);
    drop(first);
    drop(second);
    first_task.await.unwrap().unwrap();
    second_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn finite_replay_reports_gaps_and_survives_reopen() {
    let (_temp, manager) = manager().await;
    seed(&manager, 1005).await;
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
    assert_eq!((oldest_id, latest_id), (6, 1005));
    let count = latest_id - oldest_id + 1;
    for index in 0..count {
        let Body::EventItem(EventItem::Event(event)) = receive(&mut stream).await else {
            panic!("expected retained event");
        };
        assert_eq!(event.id, oldest_id + index);
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
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    for follow in [false, true] {
        let mut method = Method::WatchWorkspaceEvents {
            since: Some(0),
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
    manager.observe_workspace_branch(&workspace).await.unwrap();
    git(&workspace.path, &["checkout", "--detach"]);
    manager.observe_workspace_branch(&workspace).await.unwrap();
    assert_eq!(
        manager.workspace(&workspace.id).await.unwrap().branch,
        "worker"
    );
    let events = manager.workspace_events(None, 100).await.unwrap();
    let changed: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EventItem::Event(e) if e.details.kind == EventKind::BranchChanged => {
                Some(e.details.branch.as_deref())
            }
            _ => None,
        })
        .collect();
    assert_eq!(changed, [Some("other"), None]);
}

#[tokio::test]
async fn removal_causes_and_retained_errors_survive_ownership_and_notification_reads() {
    use crate::{
        forge::pr::Action,
        removal::{BranchChoice, InspectionPolicy},
    };
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    for (name, cause) in [
        ("manual", EventCause::Manual),
        ("idle", EventCause::Idle),
        ("issue", EventCause::Issue),
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
                    .remove_workspace(&workspace.id, BranchChoice::Auto, InspectionPolicy::GitOnly)
                    .await
                    .unwrap();
            }
            EventCause::Idle => {
                let snapshot = manager
                    .cleanup_snapshot(&workspace.id)
                    .await
                    .unwrap()
                    .unwrap();
                manager.remove_idle(&workspace.id, snapshot).await.unwrap();
            }
            EventCause::Issue => {
                let head = crate::forge::pr::current_head(&workspace).await.unwrap();
                manager
                    .record_done(&workspace, head, Some(true), EventCause::Issue)
                    .await
                    .unwrap();
                manager.sweep_completed().await.unwrap();
            }
            EventCause::Pr => {
                manager
                    .set_pr(&workspace.id, Action::Acknowledge)
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
        assert!(manager.workspace(&workspace.id).await.is_err());
        let events = manager.workspace_events(None, 100).await.unwrap();
        assert!(events.iter().any(|e| matches!(e, EventItem::Event(e) if e.details.workspace_id == workspace.id && e.details.kind == EventKind::Removed && e.details.cause == Some(cause))));
    }
    let workspace = manager
        .create_workspace(&repo.id, "retain".into(), None, None, None)
        .await
        .unwrap();
    std::fs::write(workspace.path.join("dirty"), "keep this work").unwrap();
    manager.mark_done(&workspace.id, Some(true)).await.unwrap();
    manager.sweep_completed().await.unwrap();
    let notifications = manager.notifications(true, 100).await.unwrap();
    manager
        .mark_notifications_read(notifications.iter().map(|n| n.id).collect())
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    let events = manager.workspace_events(None, 100).await.unwrap();
    let errors: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EventItem::Event(e)
                if e.details.workspace_id == workspace.id
                    && e.details.kind == EventKind::Retained =>
            {
                Some(e.details.error.as_deref().unwrap())
            }
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("uncommitted or untracked"));
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
            oldest_id: 1,
            latest_id: 0,
            ..
        })
    ));
    seed(&manager, 1).await;
    assert!(matches!(
        receive(&mut stream).await,
        Body::EventItem(EventItem::Event(event)) if event.id == 1
    ));
    drop(stream);
    task.await.unwrap().unwrap();
    seed(&manager, 1004).await;
    let boundary = manager.workspace_events(Some(5), 1000).await.unwrap();
    assert_eq!(boundary.len(), 1000);
    assert!(matches!(
        &boundary[0],
        EventItem::Event(event) if event.id == 6
    ));
    let future = manager.workspace_events(Some(1006), 1000).await.unwrap();
    assert!(matches!(
        future[0],
        EventItem::Gap {
            since: 1006,
            oldest_id: 6,
            latest_id: 1005
        }
    ));
    assert_eq!(future.len(), 1001);
    assert!(manager.workspace_events(Some(-1), 100).await.is_err());
}
