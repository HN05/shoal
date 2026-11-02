use super::*;
use crate::test_support::manager;

#[tokio::test]
async fn repeated_conflicts_collapse_until_read_and_completed_events_never_do() {
    let (_temp, manager) = manager().await;
    let watcher = manager.signals.watch_notifications();
    for _ in 0..3 {
        manager
            .notify(Some("a"), NotificationKind::ResourceBusy, "pool busy")
            .await;
    }
    manager
        .notify(Some("b"), NotificationKind::ResourceBusy, "pool busy")
        .await;
    for _ in 0..2 {
        manager
            .notify(Some("a"), NotificationKind::AgentExited, "claude exited")
            .await;
    }
    for _ in 0..2 {
        manager
            .notify(
                Some("a"),
                NotificationKind::HookFailed,
                "workspace removed; hook failed",
            )
            .await;
    }
    assert!(watcher.has_changed().unwrap());
    let unread = manager.notifications(true, 50).await.unwrap();
    let summary: Vec<_> = unread
        .iter()
        .map(|n| (n.workspace.as_deref(), n.kind, n.message.as_str()))
        .collect();
    assert_eq!(
        summary,
        [
            (Some("a"), NotificationKind::ResourceBusy, "pool busy"),
            (Some("b"), NotificationKind::ResourceBusy, "pool busy"),
            (Some("a"), NotificationKind::AgentExited, "claude exited"),
            (Some("a"), NotificationKind::AgentExited, "claude exited"),
            (
                Some("a"),
                NotificationKind::HookFailed,
                "workspace removed; hook failed"
            ),
            (
                Some("a"),
                NotificationKind::HookFailed,
                "workspace removed; hook failed"
            ),
        ]
    );
    assert_eq!(manager.unread_notifications().await.unwrap(), 6);
    // A limited unread listing is the oldest part of the backlog.
    assert_eq!(
        manager.notifications(true, 2).await.unwrap()[1].id,
        unread[1].id
    );
    let last = unread.last().unwrap().id;
    manager
        .mark_notifications_read(unread.iter().map(|n| n.id).collect())
        .await
        .unwrap();
    assert!(manager.notifications(true, 50).await.unwrap().is_empty());
    assert_eq!(manager.notifications(false, 50).await.unwrap().len(), 6);
    assert_eq!(manager.notifications(false, 1).await.unwrap()[0].id, last);
    // Once read, the same conflict is news again.
    manager
        .notify(Some("a"), NotificationKind::ResourceBusy, "pool busy")
        .await;
    let after = manager.notifications_after(last, 50).await.unwrap();
    assert_eq!(after.len(), 1);
    assert!(after[0].id > last && !after[0].read);
    // Clearing deletes read and unread ones; later ids keep increasing.
    assert_eq!(manager.clear_notifications().await.unwrap().cleared, 7);
    assert!(manager.notifications(false, 50).await.unwrap().is_empty());
    assert_eq!(manager.unread_notifications().await.unwrap(), 0);
    manager
        .notify(None, NotificationKind::AgentExited, "claude exited")
        .await;
    let next = manager.notifications_after(after[0].id, 50).await.unwrap();
    assert_eq!(next.len(), 1);
}

#[tokio::test]
async fn agent_messages_are_validated_collapsed_and_confined_to_their_workspace() {
    use crate::{
        daemon::scope::{self, Caller},
        protocol::NotificationMethod,
        test_support::repository,
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
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    for invalid in ["", "  ", "two\nlines", "bell\x07", &"x".repeat(513)] {
        assert!(
            manager
                .send_message(&workspace.id, invalid.into(), Vec::new(), &Attacher::User)
                .await
                .is_err(),
            "{invalid:?}"
        );
    }
    for _ in 0..2 {
        manager
            .send_message(
                &workspace.id,
                "PR #12 is ready to merge".into(),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    let unread = manager.notifications(true, 50).await.unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].workspace.as_deref(), Some("sender"));
    assert_eq!(unread[0].kind, NotificationKind::AgentMessage);
    assert_eq!(unread[0].message, "PR #12 is ready to merge");
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: workspace.id.clone(),
                execution: None,
            },
        )
        .await;
    for (target, allowed) in [(&workspace.name, true), (&other.name, false)] {
        for mut method in [
            NotificationMethod::SendMessage {
                space: target.clone(),
                message: "hello".into(),
                attachments: Vec::new(),
                sender_chat: None,
            },
            NotificationMethod::WithdrawMessage {
                space: target.clone(),
                id: 1,
            },
        ] {
            assert_eq!(
                scope::authorize(&manager, Some("scope"), &mut method)
                    .await
                    .is_ok(),
                allowed
            );
        }
    }
    for mut method in [
        NotificationMethod::ListNotifications {
            unread_only: true,
            limit: 50,
        },
        NotificationMethod::ClearNotifications,
    ] {
        assert!(
            scope::authorize(&manager, Some("scope"), &mut method)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn senders_withdraw_their_own_messages_until_shown() {
    use crate::test_support::repository;
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
            .send_message(&sender.id, message.into(), Vec::new(), &Attacher::User)
            .await
            .unwrap()
            .notification
            .unwrap()
    };
    let sent = send("PR #12 is ready to merge").await;
    // A repeat collapses into the same notification.
    assert_eq!(send("PR #12 is ready to merge").await, sent);
    manager
        .notify(Some("sender"), NotificationKind::ResourceBusy, "pool busy")
        .await;
    let busy = manager.notifications(true, 50).await.unwrap()[1].id;
    for (workspace, id) in [(&other.id, sent), (&sender.id, busy)] {
        let error = manager.withdraw_message(workspace, id).await.unwrap_err();
        assert!(
            error.to_string().contains("sent no notification"),
            "{error:#}"
        );
    }

    let withdrawn = manager.withdraw_message(&sender.id, sent).await.unwrap();
    assert_eq!(withdrawn.message, "PR #12 is ready to merge");
    let unread = manager.notifications(true, 50).await.unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].id, busy);

    let shown = send("PR #13 is ready to merge").await;
    manager.mark_notifications_read(vec![shown]).await.unwrap();
    let error = manager
        .withdraw_message(&sender.id, shown)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already shown"), "{error:#}");

    // The sender keeps its messages across a rename, and a later
    // workspace taking its name gets none of them.
    let unread = send("PR #14 is ready to merge").await;
    let renamed = manager
        .rename_workspace(&sender.id, "renamed", None)
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
        .send_message(
            &successor.id,
            "PR #14 is ready to merge".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap()
        .notification
        .unwrap();
    assert_ne!(own, unread);
    let error = manager
        .withdraw_message(&successor.id, unread)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("sent no notification"),
        "{error:#}"
    );
    manager.withdraw_message(&renamed.id, unread).await.unwrap();
}

#[tokio::test]
async fn a_message_stays_its_sender_s_through_a_rename_while_it_is_recorded() {
    use crate::test_support::repository;
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
    let about = About::workspace(&sender.name, &sender.id);
    manager
        .rename_workspace(&sender.id, "renamed", None)
        .await
        .unwrap();
    let path = Some(temp.path().join("successor"));
    let successor = manager
        .create_workspace(&repo.id, "sender".into(), None, None, path)
        .await
        .unwrap();
    let id = manager
        .record_notification(about, NotificationKind::AgentMessage, "ready")
        .await
        .unwrap()
        .unwrap();
    assert!(manager.withdraw_message(&successor.id, id).await.is_err());
    manager.withdraw_message(&sender.id, id).await.unwrap();
}

#[tokio::test]
async fn pruning_keeps_unread_and_the_newest_read_notifications() {
    let (_temp, manager) = manager().await;
    manager
        .notify(None, NotificationKind::WorkspaceRemoved, "keep unread")
        .await;
    for i in 0..RETAINED + 5 {
        manager
            .notify(None, NotificationKind::AgentExited, format!("exit {i}"))
            .await;
    }
    manager
        .store
        .run(|db| {
            db.execute("UPDATE notifications SET read=1 WHERE id>1", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager
        .notify(None, NotificationKind::AgentExited, "final")
        .await;
    // Read rows beyond the newest RETAINED go; the older unread one stays.
    let all = manager.notifications(false, u32::MAX).await.unwrap();
    assert_eq!(all.len(), RETAINED + 1);
    assert_eq!(all[0].message, "keep unread");
    assert_eq!(all[1].message, "exit 6");
    assert_eq!(all.iter().filter(|n| !n.read).count(), 2);
}

#[tokio::test]
async fn a_failed_state_report_alerts_one_line_that_fits() {
    use crate::test_support::repository;
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
        .report_state_failure(&workspace.id, &error)
        .await
        .unwrap();
    let unread = manager.notifications(true, 50).await.unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].kind, NotificationKind::StateReportFailed);
    assert_eq!(unread[0].workspace.as_deref(), Some("reporter"));
    assert!(crate::validate::message(&unread[0].message).is_ok());
    assert!(
        unread[0]
            .message
            .starts_with("agent state report failed; first second x"),
        "{}",
        unread[0].message
    );
}

#[tokio::test]
async fn files_sent_with_a_message_are_listed_with_it_after_removal_too() {
    use crate::{state::AgentState, test_support::repository};
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
        .split_workspace(&parent.id, "child".into(), None)
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
            .send_message(&child.id, "Which layout?".into(), attach(), &Attacher::User)
            .await
            .unwrap();
    }
    // Sending the same files again collapses with the unread message.
    let unread = manager.notifications(true, 50).await.unwrap();
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
    let attached = manager.attached_files(&child.name).await.unwrap();
    assert_eq!(
        attached
            .iter()
            .map(|f| (f.id, f.source))
            .collect::<Vec<_>>(),
        [(file.id, FileSource::Alert)]
    );

    // A coordinating agent receives the files with the message.
    manager
        .set_agent_state(&child.id, AgentState::Working, None, None)
        .await
        .unwrap();
    manager
        .set_agent_state(&parent.id, AgentState::Working, None, None)
        .await
        .unwrap();
    let sent = manager
        .send_message(&child.id, "Done".into(), attach(), &Attacher::User)
        .await
        .unwrap();
    assert_eq!(sent.notification, None);
    let received = manager.agent_messages(&parent.id).await.unwrap();
    let [message] = received.as_slice() else {
        panic!("{received:?}");
    };
    assert_eq!(message.message, "child: Done");
    let [copy] = message.attachments.as_slice() else {
        panic!("{message:?}");
    };
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "png");

    manager
        .remove_workspace(
            &child.id,
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let listed = manager.notifications(false, 50).await.unwrap();
    let kept = listed
        .iter()
        .find(|n| n.message == "Which layout?")
        .unwrap();
    assert_eq!(kept.files.len(), 1, "{kept:?}");
    assert!(kept.files[0].path.exists());
    assert!(copy.exists());
}
