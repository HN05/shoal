use super::*;
use shoal_protocol::space_names::SpaceSelector;

use crate::{scope::Caller, stand_in::Fixture};

#[tokio::test]
async fn repeated_conflicts_collapse_until_read_and_completed_events_never_do() {
    let fixture = Fixture::new().await;
    let (coordination, daemon) = (&fixture.coordination, &*fixture.daemon);
    let watcher = fixture.notifications.watch();
    for _ in 0..3 {
        coordination
            .notify(
                daemon,
                Some(&"a".into()),
                AlertKind::ResourceBusy,
                "pool busy",
            )
            .await;
    }
    coordination
        .notify(
            daemon,
            Some(&"b".into()),
            AlertKind::ResourceBusy,
            "pool busy",
        )
        .await;
    for _ in 0..2 {
        coordination
            .notify(
                daemon,
                Some(&"a".into()),
                AlertKind::AgentExited,
                "claude exited",
            )
            .await;
    }
    for _ in 0..2 {
        coordination
            .notify(
                daemon,
                Some(&"a".into()),
                AlertKind::HookFailed,
                "workspace removed; hook failed",
            )
            .await;
    }
    assert!(watcher.has_changed().unwrap());
    let unread = fixture.notifications.list(true, 50).await.unwrap();
    let summary: Vec<_> = unread
        .iter()
        .map(|n| (n.space.as_deref(), n.kind, n.message.as_str()))
        .collect();
    assert_eq!(
        summary,
        [
            (Some("a"), AlertKind::ResourceBusy, "pool busy"),
            (Some("b"), AlertKind::ResourceBusy, "pool busy"),
            (Some("a"), AlertKind::AgentExited, "claude exited"),
            (Some("a"), AlertKind::AgentExited, "claude exited"),
            (
                Some("a"),
                AlertKind::HookFailed,
                "workspace removed; hook failed"
            ),
            (
                Some("a"),
                AlertKind::HookFailed,
                "workspace removed; hook failed"
            ),
        ]
    );
    assert_eq!(fixture.notifications.unread().await.unwrap(), 6);
    // A limited unread listing is the oldest part of the backlog.
    assert_eq!(
        fixture.notifications.list(true, 2).await.unwrap()[1].id,
        unread[1].id
    );
    let last = unread.last().unwrap().id;
    fixture
        .notifications
        .mark_read(unread.iter().map(|n| n.id).collect())
        .await
        .unwrap();
    assert!(
        fixture
            .notifications
            .list(true, 50)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture.notifications.list(false, 50).await.unwrap().len(),
        6
    );
    assert_eq!(
        fixture.notifications.list(false, 1).await.unwrap()[0].id,
        last
    );
    // Once read, the same conflict is news again.
    coordination
        .notify(
            daemon,
            Some(&"a".into()),
            AlertKind::ResourceBusy,
            "pool busy",
        )
        .await;
    let past = |here| shoal_protocol::alerts::AlertCursor {
        here,
        ..Default::default()
    };
    let after = fixture.notifications.after(&past(last), 50).await.unwrap();
    assert_eq!(after.len(), 1);
    assert!(after[0].id > last && !after[0].read);
    // Clearing deletes read and unread ones; later ids keep increasing.
    assert_eq!(fixture.notifications.clear().await.unwrap().cleared, 7);
    assert!(
        fixture
            .notifications
            .list(false, 50)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.notifications.unread().await.unwrap(), 0);
    coordination
        .notify(daemon, None, AlertKind::AgentExited, "claude exited")
        .await;
    let next = (fixture.notifications.after(&past(after[0].id), 50).await).unwrap();
    assert_eq!(next.len(), 1);
}

#[tokio::test]
async fn a_failed_state_report_alerts_one_line_that_fits() {
    let fixture = &Fixture::new().await;
    let workspace = fixture.workspace("reporter", None).await;
    let error = format!("first\nsecond {}", "x".repeat(600));
    fixture
        .coordination
        .report_state_failure(&**fixture, &SpaceSelector::id(&workspace.id), &error)
        .await
        .unwrap();
    let unread = fixture.notifications.list(true, 50).await.unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].kind, AlertKind::StateReportFailed);
    assert_eq!(unread[0].space.as_deref(), Some("reporter"));
    assert!(crate::validate::message(&unread[0].message).is_ok());
    assert!(
        unread[0].message.contains("first second x"),
        "{}",
        unread[0].message
    );
}

#[tokio::test]
async fn an_event_named_alone_takes_no_workspace_that_shares_its_name() {
    let fixture = &Fixture::new().await;
    fixture.workspace("fix", None).await;

    // A launch that opened no workspace is named alone.
    (fixture.coordination.notify(
        &**fixture,
        Some(&"fix".into()),
        AlertKind::QueueFailed,
        "did not start",
    ))
    .await;

    let [notification] = &fixture.notifications.list(true, 10).await.unwrap()[..] else {
        panic!("one notification");
    };
    assert_eq!(notification.space.as_deref(), Some("fix"));
    assert_eq!(notification.space_id, None);
}

/// The alerts each entry of the log of `space` names as sent.
async fn logged_alerts(fixture: &Fixture, space: &SpaceId) -> Vec<Vec<i64>> {
    let id = space.clone();
    let logged = fixture.store().run(move |db| {
        let mut statement = db.prepare(
            "SELECT json_extract(record,'$.alert.alerts') FROM space_events
             WHERE space_id=?1 AND json_extract(record,'$.alert') IS NOT NULL ORDER BY rowid",
        )?;
        let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    });
    (logged.await.unwrap().iter())
        .map(|alerts| serde_json::from_str(alerts).unwrap())
        .collect()
}

#[tokio::test]
async fn a_space_s_log_names_each_alert_it_sent_up_or_down() {
    use shoal_protocol::protocol::AlertTarget;
    let fixture = Fixture::new().await;
    let parent = fixture.workspace("parent", None).await;
    let child = fixture.workspace("child", Some(&parent.id)).await;
    let send = async |from: Option<&SpaceId>, to: AlertTarget, text: &str| {
        let caller = from.map(|from| Caller::of_space(from.clone()));
        let sent = (fixture.coordination).send_alert(
            &*fixture.daemon,
            caller.as_ref(),
            to,
            text.into(),
            Vec::new(),
        );
        sent.await.unwrap().alerts
    };
    let to_child = || AlertTarget::Space {
        space: SpaceSelector::id(&child.id),
    };
    let down = send(Some(&parent.id), to_child(), "Stop the dev server").await;
    // The user's alert is no space's to log.
    send(None, to_child(), "Rebase onto main").await;
    let up = send(Some(&child.id), AlertTarget::Up, "Done").await;
    assert_eq!(logged_alerts(&fixture, &parent.id).await, [down]);
    assert_eq!(logged_alerts(&fixture, &child.id).await, [up]);
}
