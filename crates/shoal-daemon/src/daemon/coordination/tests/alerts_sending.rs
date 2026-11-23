use crate::daemon::alerts::AlertKind;
use crate::{
    daemon::{
        manager::Manager,
        test_support::{caller_in, manager, repository},
    },
    model::Workspace,
    protocol::{AlertTarget, AlertsFor},
};
use shoal_coordination::alerts::*;
use shoal_protocol::files::NewFile;
use shoal_protocol::space_names::SpaceSelector;

/// A parent workspace, a child below it and a sibling beside it.
async fn tree(temp: &tempfile::TempDir, manager: &Manager) -> (Workspace, Workspace, Workspace) {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .spaces
        .create_child(
            manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let sibling = manager
        .create_workspace(&repo.id, "sibling".into(), None, None, None)
        .await
        .unwrap();
    (parent, child, sibling)
}

/// Send `text` from `from`, a space or the user, down to `to`.
async fn send(manager: &Manager, from: Option<&Workspace>, to: &Workspace, text: &str) -> i64 {
    let caller = from.map(|from| caller_in(&SpaceSelector::id(&from.id)));
    let to = AlertTarget::Space {
        space: SpaceSelector::id(&to.id),
    };
    let sent =
        manager
            .coordination
            .send_alert(manager, caller.as_ref(), to, text.into(), Vec::new());
    sent.await.unwrap().alerts[0]
}

#[tokio::test]
async fn an_alert_names_its_sender_who_alone_withdraws_it_until_read() {
    let (temp, manager) = manager().await;
    let (parent, child, sibling) = tree(&temp, &manager).await;
    let from_user = send(&manager, None, &child, "Rebase onto main").await;
    let from_parent = send(&manager, Some(&parent), &child, "Stop the dev server").await;
    let unread = manager
        .agents
        .unread_messages(&child.id)
        .await
        .unwrap()
        .alerts;
    let senders: Vec<_> = (unread.iter())
        .map(|alert| (alert.sender, alert.space_id.as_ref()))
        .collect();
    assert_eq!(
        senders,
        [(Sender::User, None), (Sender::Space, Some(&parent.id))]
    );
    // Only the sender withdraws: the user's alert, not the parent's.
    let as_sibling = caller_in(&SpaceSelector::id(&sibling.id));
    for (caller, id) in [(Some(&as_sibling), from_parent), (None, from_parent)] {
        assert!(
            manager
                .coordination
                .withdraw_alert(&*manager, caller, None, id)
                .await
                .is_err()
        );
    }
    // Naming the space it went to finds it there alone.
    let (to_child, to_sibling) = (SpaceSelector::id(&child.id), SpaceSelector::id(&sibling.id));
    let withdrawn =
        manager
            .coordination
            .withdraw_alert(&*manager, None, Some(&to_sibling), from_user);
    assert!(withdrawn.await.is_err());
    let as_parent = caller_in(&SpaceSelector::id(&parent.id));
    (manager
        .coordination
        .withdraw_alert(&*manager, None, Some(&to_child), from_user))
    .await
    .unwrap();
    // Once read, it stays.
    (manager
        .coordination
        .mark_alerts_read(&*manager, None, vec![from_parent], Default::default()))
    .await
    .unwrap();
    assert!(
        manager
            .coordination
            .withdraw_alert(&*manager, Some(&as_parent), None, from_parent)
            .await
            .is_err()
    );
    let left = manager
        .coordination
        .alerts_for(
            &*manager,
            None,
            AlertsFor::Space {
                space: SpaceSelector::id(&child.id),
            },
            false,
            50,
        )
        .await
        .unwrap();
    assert_eq!(
        left.iter()
            .map(|alert| (alert.id, alert.read))
            .collect::<Vec<_>>(),
        [(from_parent, true)]
    );
}

#[tokio::test]
async fn a_scoped_caller_reads_and_marks_only_the_alerts_its_scope_reaches() {
    let (temp, manager) = manager().await;
    let (parent, child, sibling) = tree(&temp, &manager).await;
    let below = send(&manager, None, &child, "below").await;
    let beside = send(&manager, None, &sibling, "beside").await;
    (manager
        .coordination
        .notify(&*manager, None, AlertKind::ResourceBusy, "pool busy"))
    .await;
    let as_parent = caller_in(&SpaceSelector::id(&parent.id));
    let every = manager
        .coordination
        .alerts_for(&*manager, Some(&as_parent), AlertsFor::Every, false, 50)
        .await
        .unwrap();
    // The user's alerts take the `notifications` permission, which a
    // workspace's agent lacks by default.
    assert_eq!(
        every.iter().map(|alert| alert.id).collect::<Vec<_>>(),
        [below]
    );
    let user = manager
        .coordination
        .alerts_for(&*manager, None, AlertsFor::Every, false, 50)
        .await
        .unwrap();
    assert_eq!(user.len(), 3);
    assert!(
        (manager.coordination.mark_alerts_read(
            &*manager,
            Some(&as_parent),
            vec![beside],
            Default::default()
        ))
        .await
        .is_err()
    );
    (manager.coordination.mark_alerts_read(
        &*manager,
        Some(&as_parent),
        vec![below],
        Default::default(),
    ))
    .await
    .unwrap();
    assert!(
        manager
            .agents
            .unread_messages(&child.id)
            .await
            .unwrap()
            .alerts
            .is_empty()
    );
}

#[tokio::test]
async fn files_alone_go_up_as_an_alert_and_text_alone_must_not_be_empty() {
    let (temp, manager) = manager().await;
    let (parent, child, _) = tree(&temp, &manager).await;
    // Both run agents, so the child's alert goes to its coordinator.
    for space in [&parent, &child] {
        let working = crate::state::AgentState::Working;
        (manager.agents.set_agent_state(
            &*manager,
            &SpaceSelector::id(&space.id),
            working,
            None,
            None,
        ))
        .await
        .unwrap();
    }
    let file = temp.path().join("report.txt");
    std::fs::write(&file, "findings").unwrap();
    let as_child = caller_in(&SpaceSelector::id(&child.id));
    let up = |attachments: Vec<NewFile>| {
        manager.coordination.send_alert(
            &*manager,
            Some(&as_child),
            AlertTarget::Up,
            String::new(),
            attachments,
        )
    };
    assert!(up(Vec::new()).await.is_err());
    let sent = up(vec![file.clone().into()]).await.unwrap();
    let unread = manager
        .agents
        .unread_messages(&parent.id)
        .await
        .unwrap()
        .alerts;
    let [alert] = unread.as_slice() else {
        panic!("{unread:?}");
    };
    assert_eq!((alert.id, alert.message.as_str()), (sent.alerts[0], ""));
    let attachments = alert.attachments();
    let [copy] = attachments.as_slice() else {
        panic!("{alert:?}");
    };
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "findings");
}

/// The numbers the `alert_sent` entries of `space`'s log name, oldest
/// first.
async fn logged_alerts(manager: &Manager, space: &Workspace) -> Vec<Vec<i64>> {
    let (selector, filter) = (
        SpaceSelector::id(&space.id),
        shoal_protocol::log::LogFilter::Entries,
    );
    let log = manager.space_log(&selector, filter, 50).await.unwrap();
    (log.into_iter())
        .filter_map(|entry| match entry {
            shoal_protocol::log::LogEntry::Event(event) => event.details.alert,
            _ => None,
        })
        .map(|alert| alert.alerts)
        .collect()
}

#[tokio::test]
async fn a_space_s_log_names_each_alert_it_sent_up_or_down() {
    let (temp, manager) = manager().await;
    let (parent, child, _) = tree(&temp, &manager).await;
    let down = send(&manager, Some(&parent), &child, "Stop the dev server").await;
    // The user's alert is no space's to log.
    send(&manager, None, &child, "Rebase onto main").await;
    let as_child = caller_in(&SpaceSelector::id(&child.id));
    let up = manager.coordination.send_alert(
        &*manager,
        Some(&as_child),
        AlertTarget::Up,
        "Done".into(),
        Vec::new(),
    );
    let up = up.await.unwrap().alerts;
    assert_eq!(logged_alerts(&manager, &parent).await, [vec![down]]);
    assert_eq!(logged_alerts(&manager, &child).await, [up]);
}
