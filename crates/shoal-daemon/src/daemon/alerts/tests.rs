use super::*;
use crate::daemon::test_support::{away_on_offline_machine, held_calls, manager};
use crate::daemon::workspace_files::{FileSource, NewFile};
use shoal_protocol::{hub::routing::Queued, space_names::SpaceSelector};

#[tokio::test]
async fn repeated_conflicts_collapse_until_read_and_completed_events_never_do() {
    let (_temp, manager) = manager().await;
    let watcher = manager.notifications.watch();
    for _ in 0..3 {
        manager
            .notify(Some(&"a".into()), AlertKind::ResourceBusy, "pool busy")
            .await;
    }
    manager
        .notify(Some(&"b".into()), AlertKind::ResourceBusy, "pool busy")
        .await;
    for _ in 0..2 {
        manager
            .notify(Some(&"a".into()), AlertKind::AgentExited, "claude exited")
            .await;
    }
    for _ in 0..2 {
        manager
            .notify(
                Some(&"a".into()),
                AlertKind::HookFailed,
                "workspace removed; hook failed",
            )
            .await;
    }
    assert!(watcher.has_changed().unwrap());
    let unread = manager.notifications.list(true, 50).await.unwrap();
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
    assert_eq!(manager.notifications.unread().await.unwrap(), 6);
    // A limited unread listing is the oldest part of the backlog.
    assert_eq!(
        manager.notifications.list(true, 2).await.unwrap()[1].id,
        unread[1].id
    );
    let last = unread.last().unwrap().id;
    manager
        .notifications
        .mark_read(unread.iter().map(|n| n.id).collect())
        .await
        .unwrap();
    assert!(
        manager
            .notifications
            .list(true, 50)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        manager.notifications.list(false, 50).await.unwrap().len(),
        6
    );
    assert_eq!(
        manager.notifications.list(false, 1).await.unwrap()[0].id,
        last
    );
    // Once read, the same conflict is news again.
    manager
        .notify(Some(&"a".into()), AlertKind::ResourceBusy, "pool busy")
        .await;
    let after = manager.notifications.after(last, 50).await.unwrap();
    assert_eq!(after.len(), 1);
    assert!(after[0].id > last && !after[0].read);
    // Clearing deletes read and unread ones; later ids keep increasing.
    assert_eq!(manager.notifications.clear().await.unwrap().cleared, 7);
    assert!(
        manager
            .notifications
            .list(false, 50)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(manager.notifications.unread().await.unwrap(), 0);
    manager
        .notify(None, AlertKind::AgentExited, "claude exited")
        .await;
    let next = manager.notifications.after(after[0].id, 50).await.unwrap();
    assert_eq!(next.len(), 1);
}

#[tokio::test]
async fn agent_messages_are_validated_collapsed_and_confined_to_their_workspace() {
    use crate::{
        daemon::{
            scope::{self, Caller},
            test_support::repository,
        },
        protocol::AlertMethod,
    };
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "sender".into(), None, None, None)
        .await
        .unwrap();
    for invalid in ["", "  ", "two\nlines", "bell\x07", &"x".repeat(513)] {
        assert!(
            manager
                .send_alert(
                    Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                        &workspace.id
                    ))),
                    crate::protocol::AlertTarget::Up,
                    invalid.into(),
                    Vec::new(),
                )
                .await
                .is_err(),
            "{invalid:?}"
        );
    }
    for _ in 0..2 {
        manager
            .send_alert(
                Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                    &workspace.id,
                ))),
                crate::protocol::AlertTarget::Up,
                "PR #12 is ready to merge".into(),
                Vec::new(),
            )
            .await
            .unwrap();
    }
    let unread = manager.notifications.list(true, 50).await.unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].space.as_deref(), Some("sender"));
    assert_eq!(unread[0].kind, AlertKind::Message);
    assert_eq!(unread[0].message, "PR #12 is ready to merge");
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: workspace.id.clone(),
                execution: None,
                granted: None,
                read_only: false,
                vouched: None,
            },
        )
        .await;
    // A process sends alerts up and withdraws the ones it sent; the daemon
    // tells which by its scope.
    for mut method in [
        AlertMethod::SendAlert {
            from: None,
            to: crate::protocol::AlertTarget::Up,
            message: "hello".into(),
            attachments: Vec::new(),
        },
        AlertMethod::WithdrawAlert {
            id: 1,
            space: None,
            from: None,
        },
    ] {
        assert!(
            scope::authorize(&manager, Some(&"scope".into()), &mut method)
                .await
                .is_ok()
        );
    }
    for mut method in [
        AlertMethod::ListAlerts {
            to: crate::protocol::AlertsFor::User,
            unread_only: true,
            limit: 50,
        },
        AlertMethod::ClearAlerts,
    ] {
        assert!(
            scope::authorize(&manager, Some(&"scope".into()), &mut method)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn senders_withdraw_their_own_messages_until_shown() {
    use crate::daemon::test_support::repository;
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let sender = manager
        .create_workspace(&repo.id, "sender".into(), None, None, None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let send = async |message: &str| {
        manager
            .send_alert(
                Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                    &sender.id,
                ))),
                crate::protocol::AlertTarget::Up,
                message.into(),
                Vec::new(),
            )
            .await
            .unwrap()
            .alerts[0]
    };
    let sent = send("PR #12 is ready to merge").await;
    // A repeat collapses into the same notification.
    assert_eq!(send("PR #12 is ready to merge").await, sent);
    manager
        .notify(Some(&"sender".into()), AlertKind::ResourceBusy, "pool busy")
        .await;
    let busy = manager.notifications.list(true, 50).await.unwrap()[1].id;
    for (workspace, id) in [(&other.id, sent), (&sender.id, busy)] {
        assert!(
            manager
                .withdraw_alert(
                    Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                        workspace
                    ))),
                    None,
                    id
                )
                .await
                .is_err()
        );
    }

    let withdrawn = manager
        .withdraw_alert(
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &sender.id,
            ))),
            None,
            sent,
        )
        .await
        .unwrap();
    assert_eq!(withdrawn.message, "PR #12 is ready to merge");
    let unread = manager.notifications.list(true, 50).await.unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].id, busy);

    let shown = send("PR #13 is ready to merge").await;
    manager.notifications.mark_read(vec![shown]).await.unwrap();
    assert!(
        manager
            .withdraw_alert(
                Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                    &sender.id
                ))),
                None,
                shown
            )
            .await
            .is_err()
    );

    // The sender keeps its messages across a rename, and a later
    // workspace taking its name gets none of them.
    let unread = send("PR #14 is ready to merge").await;
    let renamed = manager
        .rename_branch(&SpaceSelector::id(&sender.id), "renamed", None)
        .await
        .unwrap();
    // The renamed workspace keeps its directory, so the new one needs another.
    let path = Some(temp.path().join("successor"));
    let successor = manager
        .create_workspace(&repo.id, "sender".into(), None, None, path)
        .await
        .unwrap();
    // The same message from it is its own, not collapsed into the earlier one.
    let own = manager
        .send_alert(
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &successor.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "PR #14 is ready to merge".into(),
            Vec::new(),
        )
        .await
        .unwrap()
        .alerts[0];
    assert_ne!(own, unread);
    assert!(
        manager
            .withdraw_alert(
                Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                    &successor.id
                ))),
                None,
                unread
            )
            .await
            .is_err()
    );
    manager
        .withdraw_alert(
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &renamed.id,
            ))),
            None,
            unread,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_message_stays_its_sender_s_through_a_rename_while_it_is_recorded() {
    use crate::daemon::test_support::repository;
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let sender = manager
        .create_workspace(&repo.id, "sender".into(), None, None, None)
        .await
        .unwrap();
    // The sender is resolved, then renamed and its name taken before
    // the message is recorded.
    let about = About::space(&sender.name, &sender.id);
    manager
        .rename_branch(&SpaceSelector::id(&sender.id), "renamed", None)
        .await
        .unwrap();
    let path = Some(temp.path().join("successor"));
    let successor = manager
        .create_workspace(&repo.id, "sender".into(), None, None, path)
        .await
        .unwrap();
    let id = manager
        .record_sent(crate::daemon::alerts::sent_alert(
            about,
            "ready".into(),
            &[],
        ))
        .await
        .unwrap()
        .alerts[0];
    assert!(
        manager
            .withdraw_alert(
                Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                    &successor.id
                ))),
                None,
                id
            )
            .await
            .is_err()
    );
    manager
        .withdraw_alert(
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &sender.id,
            ))),
            None,
            id,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_failed_state_report_alerts_one_line_that_fits() {
    use crate::daemon::test_support::repository;
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "reporter".into(), None, None, None)
        .await
        .unwrap();
    let error = format!("first\nsecond {}", "x".repeat(600));
    manager
        .report_state_failure(&SpaceSelector::id(&workspace.id), &error)
        .await
        .unwrap();
    let unread = manager.notifications.list(true, 50).await.unwrap();
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
async fn files_sent_with_a_message_are_listed_with_it_after_removal_too() {
    use crate::{daemon::test_support::repository, state::AgentState};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .create_child(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let shot = temp.path().join("layout.png");
    std::fs::write(&shot, "png").unwrap();
    let attach = || {
        vec![NewFile {
            path: shot.clone(),
            title: Some("Layout".into()),
        }]
    };
    for _ in 0..2 {
        manager
            .send_alert(
                Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                    &child.id,
                ))),
                crate::protocol::AlertTarget::Up,
                "Which layout?".into(),
                attach(),
            )
            .await
            .unwrap();
    }
    // Sending the same files again collapses with the unread message.
    let unread = manager.notifications.list(true, 50).await.unwrap();
    let [sent] = unread.as_slice() else {
        panic!("{unread:?}");
    };
    let [file] = sent.files.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(
        (file.name.as_str(), file.title.as_deref()),
        ("layout.png", Some("Layout"))
    );
    assert_eq!(std::fs::read_to_string(&file.path).unwrap(), "png");
    let attached = manager
        .attached_files(&SpaceSelector::id(&child.id))
        .await
        .unwrap();
    assert_eq!(
        attached
            .iter()
            .map(|f| (f.id, f.source))
            .collect::<Vec<_>>(),
        [(file.id, FileSource::Alert)]
    );

    // A coordinating agent receives the files with the message.
    manager
        .set_agent_state(
            &SpaceSelector::id(&child.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .set_agent_state(
            &SpaceSelector::id(&parent.id),
            AgentState::Working,
            None,
            None,
        )
        .await
        .unwrap();
    let sent = manager
        .send_alert(
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &child.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "Done".into(),
            attach(),
        )
        .await
        .unwrap();
    assert!(!sent.user);
    let received = manager
        .agent_messages(&SpaceSelector::id(&parent.id))
        .await
        .unwrap();
    let [message] = received.as_slice() else {
        panic!("{received:?}");
    };
    assert_eq!(message.text(), format!("{} child: Done", child.number));
    let attachments = message.attachments();
    let [copy] = attachments.as_slice() else {
        panic!("{message:?}");
    };
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "png");

    manager
        .remove_selected(
            &SpaceSelector::id(&child.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let listed = manager.notifications.list(false, 50).await.unwrap();
    let kept = listed
        .iter()
        .find(|n| n.message == "Which layout?")
        .unwrap();
    assert_eq!(kept.files.len(), 1, "{kept:?}");
    assert!(kept.files[0].path.exists());
    assert!(copy.exists());
}

#[tokio::test]
async fn a_message_to_the_user_sends_a_limited_number_of_files() {
    use crate::daemon::test_support::repository;
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "sender".into(), None, None, None)
        .await
        .unwrap();
    let shot = temp.path().join("shot.png");
    std::fs::write(&shot, "png").unwrap();
    let files = vec![NewFile::from(shot); shoal_protocol::files::MAX_SENT + 1];
    let sent = manager
        .send_alert(
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &workspace.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "Look".into(),
            files,
        )
        .await;
    assert!(sent.is_err());
    assert!(
        manager
            .attached_files(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        manager
            .notifications
            .list(false, 50)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_event_named_alone_takes_no_workspace_that_shares_its_name() {
    let (temp, manager) = manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "fix".into(), None, None, None)
        .await
        .unwrap();

    // A launch that opened no workspace is named alone.
    (manager.notify(Some(&"fix".into()), AlertKind::QueueFailed, "did not start")).await;

    let [notification] = &manager.notifications.list(true, 10).await.unwrap()[..] else {
        panic!("one notification");
    };
    assert_eq!(notification.space.as_deref(), Some("fix"));
    assert_eq!(notification.space_id, None);
}

#[tokio::test]
async fn a_message_to_another_machine_s_space_waits_at_the_hub_while_that_machine_is_offline() {
    let (_temp, manager) = manager().await;
    away_on_offline_machine(&manager, None).await;
    let to = SpaceSelector::Number(90.into());
    let sent = manager.send_space_message(None, &to, "Rebase onto main".into(), Vec::new());
    let error = sent.await.unwrap_err();
    let queued = error.downcast_ref::<Queued>().expect("the hub holds it");
    assert_eq!(queued.name, "desk");
    let held = held_calls(&manager).await;
    let [call] = held.as_slice() else {
        panic!("the hub holds {held:?}");
    };
    assert!(
        call.contains("send_alert") && call.contains("Rebase onto main"),
        "{call}"
    );
}

#[tokio::test]
async fn a_message_to_another_machine_s_space_takes_no_files_of_this_one() {
    let (temp, manager) = manager().await;
    away_on_offline_machine(&manager, None).await;
    let file = temp.path().join("report.txt");
    std::fs::write(&file, "draft").unwrap();
    let to = SpaceSelector::Number(90.into());
    let attachments = vec![NewFile::from(file)];
    let sent = manager.send_space_message(None, &to, "See the report".into(), attachments);
    let error = sent.await.unwrap_err().to_string();
    assert!(error.contains("machine desk"), "{error}");
    assert!(held_calls(&manager).await.is_empty());
}
