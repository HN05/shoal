use shoal_client::{Record, records::Repository};

use super::*;

fn repository(id: &str, name: &str) -> ((String, String), Record) {
    let record = Record::Repository(Repository {
        id: id.into(),
        name: Some(name.into()),
        path: "/repo".into(),
        source: "/repo".into(),
    });
    (("repository".into(), id.into()), record)
}

#[test]
fn changes_name_each_new_changed_and_removed_record() {
    let before: Records = [
        repository("a", "a"),
        repository("b", "b"),
        repository("c", "c"),
    ]
    .into_iter()
    .collect();
    let after: Records = [
        repository("a", "a"),
        repository("b", "renamed"),
        repository("d", "d"),
    ]
    .into_iter()
    .collect();
    let events = changes(&before, &after);
    assert_eq!(
        events,
        [
            Event::Changed {
                record: repository("b", "renamed").1
            },
            Event::Changed {
                record: repository("d", "d").1
            },
            Event::Removed {
                key: snapshot::key("repository", "c")
            },
        ]
    );
}

mod streams {
    use shoal_client::{
        Subscription, Topic, Update,
        records::{MessageRecipient, RecordKind},
    };

    use super::super::super::tests::{HANG, TestDaemon};
    use crate::{
        daemon::{notifications::NotificationKind, questions::Ask},
        forge::items::link::LinkKind,
        model::Relation,
        removal::{InspectionPolicy, UnsavedWork},
    };

    /// The first update `matches` accepts, skipping others.
    async fn until(subscription: &mut Subscription, matches: impl Fn(&Update) -> bool) -> Update {
        tokio::time::timeout(HANG, async {
            loop {
                let update = subscription.next().await.unwrap();
                if matches(&update) {
                    return update;
                }
            }
        })
        .await
        .unwrap()
    }

    /// Wait for a change of each of `kinds`, in any order.
    async fn until_each(subscription: &mut Subscription, kinds: &[RecordKind]) {
        let mut waiting = kinds.to_vec();
        while !waiting.is_empty() {
            let Update::Changed(record) =
                until(subscription, |update| matches!(update, Update::Changed(_))).await
            else {
                unreachable!();
            };
            waiting.retain(|kind| record.key().is_none_or(|key| key.kind != *kind));
        }
    }

    fn changed(kind: RecordKind) -> impl Fn(&Update) -> bool {
        move |update| matches!(update, Update::Changed(record) if record.key().is_some_and(|key| key.kind == kind))
    }

    fn ask() -> Ask {
        Ask {
            question: "Which database?".into(),
            recommendation: "SQLite".into(),
            reason: None,
            choices: Vec::new(),
        }
    }

    fn all_topics() -> Vec<Topic> {
        Topic::KNOWN
            .iter()
            .map(|topic| Topic::from(*topic))
            .collect()
    }

    #[tokio::test]
    async fn a_subscription_sends_its_snapshot_then_each_kind_of_change() {
        let daemon = TestDaemon::start().await;
        let workspace = daemon.workspace("first").await;
        let client = daemon.client().await;
        let mut subscription = client.subscribe(all_topics()).await.unwrap();
        let Update::Snapshot { records, resynced } = subscription.next().await.unwrap() else {
            panic!("a subscription starts with its snapshot");
        };
        assert!(!resynced);
        let kinds: Vec<_> = records
            .iter()
            .filter_map(|r| r.key())
            .map(|key| key.kind)
            .collect();
        assert!(kinds.contains(&RecordKind::Repository), "{kinds:?}");
        assert!(kinds.contains(&RecordKind::Workspace), "{kinds:?}");

        let manager = &daemon.manager;
        let other = crate::test_support::repository(&manager.paths.home, "other");
        manager
            .register_repository(other.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        until(&mut subscription, changed(RecordKind::Repository)).await;

        manager
            .acquire_hold(&workspace.id, "quay".into(), None)
            .await
            .unwrap();
        until(&mut subscription, |update| {
            matches!(update, Update::Changed(shoal_client::Record::Workspace(w)) if w.holds.len() == 1)
        })
        .await;

        manager
            .link_item(
                &workspace.id,
                LinkKind::Issue,
                "40",
                Relation::Related,
                None,
            )
            .await
            .unwrap();
        until(&mut subscription, changed(RecordKind::Link)).await;

        let question = manager.ask_question(&workspace.id, ask()).await.unwrap();
        until_each(
            &mut subscription,
            &[RecordKind::Question, RecordKind::Notification],
        )
        .await;

        manager
            .send_agent_message(&workspace.id, "Rebase on main".into(), Vec::new())
            .await
            .unwrap();
        until(&mut subscription, |update| {
            matches!(update, Update::Changed(shoal_client::Record::Message(m)) if m.recipient == MessageRecipient::Agent)
        })
        .await;
        manager
            .send_message(&workspace.id, "Ready for review".into())
            .await
            .unwrap();
        until(&mut subscription, |update| {
            matches!(update, Update::Changed(shoal_client::Record::Message(m)) if m.recipient == MessageRecipient::User)
        })
        .await;

        manager
            .answer_question(question.id, "Postgres".into(), None)
            .await
            .unwrap();
        until(
            &mut subscription,
            |update| matches!(update, Update::Removed(key) if key.kind == RecordKind::Question),
        )
        .await;

        manager
            .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
            .await
            .unwrap();
        until(&mut subscription, |update| {
            matches!(update, Update::Removed(key) if key.kind == RecordKind::Workspace && key.id == workspace.id)
        })
        .await;
    }

    #[tokio::test]
    async fn an_approval_request_reaches_a_subscription() {
        use crate::{
            config::repo::ConflictPolicy,
            daemon::approvals::{
                ApprovalRequest, Lifetime, PortSpecification, Specification, Target,
            },
        };
        let daemon = TestDaemon::start().await;
        let workspace = daemon.workspace("requester").await;
        let client = daemon.client().await;
        let mut subscription = client.subscribe(vec![Topic::Approvals]).await.unwrap();
        subscription.next().await.unwrap();
        let request = ApprovalRequest::new(
            &workspace.id,
            Target::Port("web".into()),
            "web",
            Specification::Port(PortSpecification {
                env: "PORT".into(),
                on_conflict: ConflictPolicy::Suggest,
                preferred: None,
                range: [4000, 4100],
            }),
            Lifetime::Workspace,
            Some("serve the app"),
        );
        daemon.manager.request_approval(request).await.unwrap();
        until(&mut subscription, changed(RecordKind::Approval)).await;
    }

    #[tokio::test]
    async fn the_user_keeps_seeing_an_issue_proposal_its_workspace_left() {
        let daemon = TestDaemon::start().await;
        let workspace = daemon.workspace("proposer").await;
        let proposal = crate::daemon::links::proposals::tests::store_issue_proposal(
            &daemon.manager,
            &workspace,
        )
        .await;
        daemon
            .manager
            .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
            .await
            .unwrap();
        let client = daemon.client().await;
        let mut subscription = client.subscribe(vec![Topic::Approvals]).await.unwrap();
        let Update::Snapshot { records, .. } = subscription.next().await.unwrap() else {
            panic!("a subscription starts with its snapshot");
        };
        let ids: Vec<_> = records
            .iter()
            .filter_map(|record| record.key())
            .map(|key| key.id)
            .collect();
        assert_eq!(ids, [proposal.id.to_string()]);
    }

    #[tokio::test]
    async fn a_scoped_subscription_covers_its_tree_without_notifications() {
        let daemon = TestDaemon::start().await;
        let own = daemon.workspace("own").await;
        let other = daemon.workspace("other").await;
        daemon.manager.ask_question(&other.id, ask()).await.unwrap();
        let elsewhere = crate::test_support::repository(&daemon.manager.paths.home, "elsewhere");
        daemon
            .manager
            .register_repository(elsewhere.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let client = daemon.scoped(&own).await;
        let mut subscription = client.subscribe(all_topics()).await.unwrap();
        let Update::Snapshot { records, .. } = subscription.next().await.unwrap() else {
            panic!("a subscription starts with its snapshot");
        };
        let keys: Vec<_> = records.iter().filter_map(|record| record.key()).collect();
        let workspaces: Vec<_> = keys
            .iter()
            .filter(|key| key.kind == RecordKind::Workspace)
            .map(|key| key.id.as_str())
            .collect();
        assert_eq!(workspaces, [own.id.as_str()]);
        let repositories: Vec<_> = keys
            .iter()
            .filter(|key| key.kind == RecordKind::Repository)
            .map(|key| key.id.as_str())
            .collect();
        assert_eq!(repositories, [daemon.repository.id.as_str()]);
        assert!(
            keys.iter()
                .all(|key| !matches!(key.kind, RecordKind::Notification | RecordKind::Question)),
            "{keys:?}"
        );
    }

    #[tokio::test]
    async fn a_large_snapshot_arrives_in_pages() {
        use shoal_client::wire::{self, Event, HandshakeReply, Reply, Response};
        let daemon = TestDaemon::start().await;
        for n in 0..200 {
            let message = format!("{n}: {}", "x".repeat(1000));
            daemon
                .manager
                .notify(None, NotificationKind::CleanupFailed, message)
                .await;
        }
        let (mut reader, mut writer) = daemon.raw().await;
        let hello = serde_json::json!({"client_api": {"version": shoal_client::VERSION}});
        wire::write(&mut writer, &hello).await.unwrap();
        let _: HandshakeReply = reader.read().await.unwrap().unwrap();
        let subscribe = serde_json::json!({"id": 1, "method": "subscribe", "params": {"topics": ["notifications"]}});
        wire::write(&mut writer, &subscribe).await.unwrap();
        let (mut pages, mut records) = (0, 0);
        loop {
            let response: Response = reader.read().await.unwrap().unwrap();
            let Reply::Event(Event::Snapshot {
                records: page,
                complete,
            }) = response.reply
            else {
                panic!("expected the snapshot: {response:?}");
            };
            pages += 1;
            records += page.len();
            if complete {
                break;
            }
        }
        assert!(pages > 1, "{pages} pages");
        assert_eq!(records, 200);
    }

    #[tokio::test]
    async fn a_daemon_quiescing_for_an_update_closes_a_subscription_instead_of_writing() {
        use shoal_client::wire::{self, HandshakeReply, Response};
        let daemon = TestDaemon::start().await;
        let workspace = daemon.workspace("kept").await;
        let (mut reader, mut writer) = daemon.raw().await;
        let hello = serde_json::json!({"client_api": {"version": shoal_client::VERSION}});
        wire::write(&mut writer, &hello).await.unwrap();
        let _: HandshakeReply = reader.read().await.unwrap().unwrap();
        let subscribe = serde_json::json!({"id": 1, "method": "subscribe", "params": {"topics": ["workspaces"]}});
        wire::write(&mut writer, &subscribe).await.unwrap();
        let _snapshot: Response = reader.read().await.unwrap().unwrap();
        let _quiesced = daemon.manager.gates.quiesce().await;
        daemon
            .manager
            .acquire_hold(&workspace.id, "quay".into(), None)
            .await
            .unwrap();
        let next = tokio::time::timeout(HANG, reader.read::<Response>())
            .await
            .unwrap();
        assert!(matches!(next, Ok(None)), "{next:?}");
    }

    #[tokio::test]
    async fn after_a_restart_the_subscription_resyncs() {
        let mut daemon = TestDaemon::start().await;
        let workspace = daemon.workspace("kept").await;
        let client = daemon.client().await;
        let mut subscription = client.subscribe(vec![Topic::Workspaces]).await.unwrap();
        subscription.next().await.unwrap();
        daemon.restart().await;
        daemon
            .manager
            .acquire_hold(&workspace.id, "quay".into(), None)
            .await
            .unwrap();
        let update = until(&mut subscription, |update| {
            matches!(update, Update::Snapshot { .. })
        })
        .await;
        let Update::Snapshot { records, resynced } = update else {
            unreachable!();
        };
        assert!(resynced);
        let Some(shoal_client::Record::Workspace(kept)) = records.first() else {
            panic!("expected the workspace: {records:?}");
        };
        assert_eq!(kept.holds.len(), 1);
    }
}
