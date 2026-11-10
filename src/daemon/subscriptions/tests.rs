use super::*;
use crate::client::subscriptions::records::{Record, RecordKey, RecordKind};

fn repository(id: &str, name: &str) -> (RecordKey, (Record, serde_json::Value)) {
    let record = Record::Repository(crate::model::Repository {
        id: id.into(),
        path: "/repo".into(),
        source: "/repo".into(),
        last_used: 0,
        name: Some(name.into()),
        workspaces_dir: None,
        contribution: false,
    });
    let value = serde_json::to_value(&record).unwrap();
    (record.key(), (record, value))
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
    let named: Vec<_> = changes(&before, &after)
        .into_iter()
        .map(|body| match body {
            Body::RecordChanged(record) => format!("changed {}", record.key().id),
            Body::RecordRemoved(key) => {
                assert_eq!(key.kind, RecordKind::Repository);
                format!("removed {}", key.id)
            }
            body => panic!("unexpected {body:?}"),
        })
        .collect();
    assert_eq!(named, ["changed b", "changed d", "removed c"]);
}

mod streams {
    use crate::client::space_ids::SpaceId;
    use crate::client::space_names::SpaceSelector;
    use crate::daemon::approvals::ApprovalRequestExt;
    use crate::execution::recovery::LastSessionExt;
    use crate::{
        client::{
            connection::Connector,
            subscriptions::{
                Subscription, Update,
                records::{MessageRecipient, Record, RecordKind, Topic},
            },
        },
        daemon::{
            notifications::NotificationKind,
            questions::Ask,
            test_daemon::{HANG, TestDaemon},
            workspace_files::Attacher,
        },
        forge::items::link::LinkKind,
        model::{HoldPlacer, Relation},
        protocol::{self, Body, ErrorCode, RemoteError, Request, SubscriptionMethod},
        removal::{InspectionPolicy, UnsavedWork},
    };

    type Subscribed = Subscription<Connector>;

    /// The first update `matches` accepts, skipping others.
    async fn until(subscription: &mut Subscribed, matches: impl Fn(&Update) -> bool) -> Update {
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
    async fn until_each(subscription: &mut Subscribed, kinds: &[RecordKind]) {
        let mut waiting = kinds.to_vec();
        while !waiting.is_empty() {
            let Update::Changed(record) =
                until(subscription, |update| matches!(update, Update::Changed(_))).await
            else {
                unreachable!();
            };
            waiting.retain(|kind| record.key().kind != *kind);
        }
    }

    fn changed(kind: RecordKind) -> impl Fn(&Update) -> bool {
        move |update| matches!(update, Update::Changed(record) if record.key().kind == kind)
    }

    fn ask() -> Ask {
        Ask {
            question: "Which database?".into(),
            recommendation: "SQLite".into(),
            reason: None,
            choices: Vec::new(),
        }
    }

    async fn subscribe(connector: Connector, topics: &[Topic]) -> Subscribed {
        Subscription::open(connector, topics.to_vec())
            .await
            .unwrap()
    }

    /// The records of the snapshot a subscription starts with.
    async fn snapshot(subscription: &mut Subscribed) -> Vec<Record> {
        let Update::Snapshot { records, resynced } = subscription.next().await.unwrap() else {
            panic!("a subscription starts with its snapshot");
        };
        assert!(!resynced);
        records
    }

    fn code(error: anyhow::Error) -> ErrorCode {
        error.downcast_ref::<RemoteError>().unwrap().code.clone()
    }

    #[tokio::test]
    async fn a_subscription_sends_its_snapshot_then_each_kind_of_change() {
        let daemon = TestDaemon::start().await;
        let workspace = daemon.workspace("first").await;
        let mut subscription = subscribe(daemon.connector(), Topic::ALL).await;
        let kinds: Vec<_> = snapshot(&mut subscription)
            .await
            .iter()
            .map(|record| record.key().kind)
            .collect();
        assert!(kinds.contains(&RecordKind::Repository), "{kinds:?}");
        assert!(kinds.contains(&RecordKind::Workspace), "{kinds:?}");

        let manager = &daemon.manager;
        let other = crate::daemon::test_support::repository(&manager.paths.home, "other");
        manager
            .register_repository(other.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        until(&mut subscription, changed(RecordKind::Repository)).await;

        manager
            .acquire_hold(
                &SpaceSelector::id(&workspace.id),
                "quay".into(),
                None,
                HoldPlacer::User,
            )
            .await
            .unwrap();
        until(&mut subscription, |update| {
            matches!(update, Update::Changed(record) if matches!(&**record, Record::Workspace(w) if w.workspace.holds.len() == 1))
        })
        .await;

        manager
            .link_item(
                &SpaceSelector::id(&workspace.id),
                LinkKind::Issue,
                "40",
                Relation::Related,
                None,
                false,
            )
            .await
            .unwrap();
        until(&mut subscription, changed(RecordKind::Link)).await;

        let question = manager
            .ask_question(
                &SpaceSelector::id(&workspace.id),
                ask(),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
        until_each(
            &mut subscription,
            &[RecordKind::Question, RecordKind::Notification],
        )
        .await;

        manager
            .send_agent_message(
                &SpaceSelector::id(&workspace.id),
                "Rebase on main".into(),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
        until(&mut subscription, |update| {
            matches!(update, Update::Changed(record) if matches!(&**record, Record::Message(m) if m.recipient == MessageRecipient::Agent))
        })
        .await;
        manager
            .send_message(
                &SpaceSelector::id(&workspace.id),
                "Ready for review".into(),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
        until(&mut subscription, |update| {
            matches!(update, Update::Changed(record) if matches!(&**record, Record::Message(m) if m.recipient == MessageRecipient::User))
        })
        .await;

        manager
            .answer_question(question.number, "Postgres".into(), None)
            .await
            .unwrap();
        until(
            &mut subscription,
            |update| matches!(update, Update::Removed(key) if key.kind == RecordKind::Question),
        )
        .await;

        manager
            .remove_workspace(
                &SpaceSelector::id(&workspace.id),
                UnsavedWork::All,
                InspectionPolicy::GitOnly,
            )
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
        let mut subscription = subscribe(daemon.connector(), &[Topic::Approvals]).await;
        snapshot(&mut subscription).await;
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
            &workspace.clone().into(),
        )
        .await;
        daemon
            .manager
            .remove_workspace(
                &SpaceSelector::id(&workspace.id),
                UnsavedWork::All,
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
        let mut subscription = subscribe(daemon.connector(), &[Topic::Approvals]).await;
        let records = snapshot(&mut subscription).await;
        let [Record::Approval(shown)] = records.as_slice() else {
            panic!("expected the proposal: {records:?}");
        };
        assert_eq!(shown.number, proposal.number);
    }

    #[tokio::test]
    async fn a_chats_subscription_follows_the_issues_it_proposed() {
        use crate::daemon::links::proposals::tests::store_issue_proposal;
        let daemon = TestDaemon::start().await;
        let chat = daemon
            .manager
            .start_chat(
                daemon.repository.id.clone().into(),
                "planning".into(),
                "claude".into(),
                std::process::id(),
                None,
                false,
            )
            .await
            .unwrap();
        let other = daemon.workspace("other").await;
        store_issue_proposal(&daemon.manager, &other.into()).await;
        let token = daemon.manager.chat_scope(&chat.id).await.unwrap();
        let connector = daemon.connector().scope(token);
        let mut subscription = subscribe(connector, &[Topic::Approvals]).await;
        let records = snapshot(&mut subscription).await;
        assert!(records.is_empty(), "{records:?}");
        let space = daemon
            .manager
            .link_space(&SpaceSelector::id(&chat.id))
            .await
            .unwrap();
        let proposal = store_issue_proposal(&daemon.manager, &space).await;
        let Update::Changed(record) = until(&mut subscription, changed(RecordKind::Approval)).await
        else {
            unreachable!();
        };
        let Record::Approval(shown) = *record else {
            unreachable!();
        };
        assert_eq!(
            (shown.number, shown.space_name()),
            (proposal.number, "planning")
        );
    }

    #[tokio::test]
    async fn the_user_sees_the_questions_of_a_chat() {
        let daemon = TestDaemon::start().await;
        let chat = daemon
            .manager
            .start_chat(
                daemon.repository.id.clone().into(),
                "planning".into(),
                "claude".into(),
                std::process::id(),
                None,
                false,
            )
            .await
            .unwrap();
        let question = daemon
            .manager
            .ask_question(
                &SpaceSelector::id(&chat.id),
                ask(),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
        let mut subscription = subscribe(daemon.connector(), &[Topic::Questions]).await;
        let records = snapshot(&mut subscription).await;
        let [Record::Question(shown)] = records.as_slice() else {
            panic!("expected the chat's question: {records:?}");
        };
        assert_eq!((shown.number, &*shown.space), (question.number, "planning"));
    }

    #[tokio::test]
    async fn a_session_an_agent_reports_reaches_a_subscription() {
        use crate::{
            client::transcripts::ReportedSession, daemon::workspace::ExecutionKind,
            execution::TrackedAgent, state::AgentState,
        };
        let daemon = TestDaemon::start().await;
        let manager = &daemon.manager;
        let workspace = daemon.workspace("work").await;
        let chat = manager
            .start_chat(
                daemon.repository.id.clone().into(),
                "planning".into(),
                "claude".into(),
                std::process::id(),
                None,
                false,
            )
            .await
            .unwrap();
        let mut subscription = subscribe(daemon.connector(), &[Topic::Sessions]).await;
        let records = snapshot(&mut subscription).await;
        assert_eq!(records.len(), 2, "{records:?}");
        let reported = |space: SpaceId, session: &'static str| {
            move |update: &Update| {
                matches!(update, Update::Changed(record) if matches!(&**record,
                    Record::Sessions(sessions) if sessions.space_id == space
                        && sessions.sessions.iter().any(|reported: &ReportedSession| reported.session == session)))
            }
        };

        let execution = manager
            .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
            .await
            .unwrap()
            .plan
            .id;
        manager
            .track_agent(
                &execution,
                &TrackedAgent::named("claude"),
                &workspace.id,
                &workspace.name,
                true,
                std::time::Duration::ZERO,
            )
            .await;
        for session in ["s1", "s2"] {
            manager
                .set_agent_state(
                    &SpaceSelector::id(&workspace.id),
                    AgentState::Idle,
                    Some(execution.clone()),
                    Some(session.into()),
                )
                .await
                .unwrap();
            until(&mut subscription, reported(workspace.id.clone(), session)).await;
        }

        manager
            .set_agent_state(
                &SpaceSelector::id(&chat.id),
                AgentState::Idle,
                None,
                Some("c1".into()),
            )
            .await
            .unwrap();
        until(&mut subscription, reported(chat.id.clone(), "c1")).await;
    }

    #[tokio::test]
    async fn a_scoped_subscription_covers_its_tree_without_notifications() {
        let daemon = TestDaemon::start().await;
        let own = daemon.workspace("own").await;
        let other = daemon.workspace("other").await;
        daemon
            .manager
            .ask_question(
                &SpaceSelector::id(&other.id),
                ask(),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
        let elsewhere =
            crate::daemon::test_support::repository(&daemon.manager.paths.home, "elsewhere");
        daemon
            .manager
            .register_repository(elsewhere.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let mut subscription = subscribe(daemon.scoped(&own).await, Topic::ALL).await;
        let keys: Vec<_> = snapshot(&mut subscription)
            .await
            .iter()
            .map(Record::key)
            .collect();
        let ids = |kind| {
            keys.iter()
                .filter(|key| key.kind == kind)
                .map(|key| key.id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(RecordKind::Workspace), [own.id.as_str()]);
        assert_eq!(ids(RecordKind::Repository), [daemon.repository.id.as_str()]);
        assert!(
            keys.iter()
                .all(|key| !matches!(key.kind, RecordKind::Notification | RecordKind::Question)),
            "{keys:?}"
        );
    }

    #[tokio::test]
    async fn a_log_subscription_follows_one_space_within_its_scope() {
        use crate::client::log::{LogEntry, LogFilter, LogTopic};
        let daemon = TestDaemon::start().await;
        let own = daemon.workspace("own").await;
        let other = daemon.workspace("other").await;
        let topic = |space: &str| LogTopic {
            space: SpaceSelector::parse(space),
            filter: LogFilter::Entries,
            limit: 50,
        };
        let refused = Subscription::follow_log(daemon.scoped(&own).await, topic("other")).await;
        assert!(refused.is_err());
        let mut subscription = Subscription::follow_log(daemon.scoped(&own).await, topic("own"))
            .await
            .unwrap();
        let records = snapshot(&mut subscription).await;
        assert!(records.iter().all(|record| matches!(
            record,
            Record::LogEntry(entry) if matches!(&**entry, LogEntry::Event(event)
                if event.details.space_id() == Some(&own.id))
        )));
        assert!(!records.is_empty());
        daemon
            .manager
            .set_workspace_title(&SpaceSelector::id(&own.id), Some("Login fix".into()))
            .await
            .unwrap();
        // Another space's entries stay out.
        daemon
            .manager
            .set_workspace_title(&SpaceSelector::id(&other.id), Some("Other".into()))
            .await
            .unwrap();
        let Update::Changed(record) = until(&mut subscription, changed(RecordKind::LogEntry)).await
        else {
            unreachable!();
        };
        let Record::LogEntry(entry) = *record else {
            unreachable!();
        };
        let LogEntry::Event(event) = *entry else {
            panic!("expected the rename");
        };
        assert_eq!(event.details.title, Some(Some("Login fix".into())));
    }

    #[tokio::test]
    async fn a_log_subscription_keeps_following_its_space_after_a_rename() {
        use crate::client::log::{LogEntry, LogFilter, LogTopic};
        let daemon = TestDaemon::start().await;
        let own = daemon.workspace("own").await;
        let topic = LogTopic {
            space: SpaceSelector::parse("own"),
            filter: LogFilter::Entries,
            limit: 50,
        };
        let mut subscription = Subscription::follow_log(daemon.connector(), topic)
            .await
            .unwrap();
        snapshot(&mut subscription).await;
        let id = own.id.clone();
        (daemon.manager.store)
            .run(move |db| {
                db.execute("UPDATE workspaces SET name='renamed' WHERE id=?1", [&id])?;
                Ok(())
            })
            .await
            .unwrap();
        let Update::Changed(record) = until(&mut subscription, changed(RecordKind::LogEntry)).await
        else {
            unreachable!();
        };
        let Record::LogEntry(entry) = *record else {
            unreachable!();
        };
        let LogEntry::Event(event) = *entry else {
            panic!("expected the rename");
        };
        assert_eq!(event.details.name, "renamed");
    }

    #[tokio::test]
    async fn a_scoped_subscription_without_workspace_records_names_its_tree() {
        let daemon = TestDaemon::start().await;
        let own = daemon.workspace("own").await;
        let other = daemon.workspace("other").await;
        for workspace in [&own, &other] {
            daemon
                .manager
                .ask_question(
                    &SpaceSelector::id(&workspace.id),
                    ask(),
                    Vec::new(),
                    &Attacher::User,
                )
                .await
                .unwrap();
            daemon
                .manager
                .send_agent_message(
                    &SpaceSelector::id(&workspace.id),
                    "Rebase on main".into(),
                    Vec::new(),
                    &Attacher::User,
                )
                .await
                .unwrap();
        }
        let topics = [Topic::Questions, Topic::Messages];
        let mut subscription = subscribe(daemon.scoped(&own).await, &topics).await;
        let records = snapshot(&mut subscription).await;
        assert_eq!(records.len(), 2, "{records:?}");
        for record in &records {
            match record {
                Record::Question(question) => assert_eq!(question.space_id, own.id),
                Record::Message(message) => {
                    assert_eq!(message.workspace_id.as_ref(), Some(&own.id));
                    assert_eq!(message.workspace.as_deref(), Some(own.name.as_str()));
                }
                other => panic!("unexpected record: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_revoked_scope_token_ends_its_subscriptions() {
        let daemon = TestDaemon::start().await;
        let workspace = daemon.workspace("revoked").await;
        let token = daemon.scope_token(&workspace).await;
        let connector = || daemon.connector().scope(token.clone());
        let mut subscription = subscribe(connector(), &[Topic::Workspaces]).await;
        snapshot(&mut subscription).await;
        daemon
            .manager
            .revoke_workspace_environment(&SpaceSelector::id(&workspace.id), token.clone())
            .await
            .unwrap();
        let refused = Subscription::open(connector(), vec![Topic::Workspaces]).await;
        assert_eq!(code(refused.err().unwrap()), ErrorCode::ScopeDenied);
        daemon
            .manager
            .acquire_hold(
                &SpaceSelector::id(&workspace.id),
                "quay".into(),
                None,
                HoldPlacer::User,
            )
            .await
            .unwrap();
        let ended = tokio::time::timeout(HANG, subscription.next())
            .await
            .unwrap();
        assert_eq!(code(ended.err().unwrap()), ErrorCode::ScopeDenied);
    }

    #[tokio::test]
    async fn discarding_a_stopped_session_reaches_a_subscription() {
        let daemon = TestDaemon::start().await;
        let workspace = daemon.workspace("paused").await;
        crate::execution::recovery::LastSession::save(
            &daemon.manager.paths,
            &workspace.id,
            &crate::execution::TrackedAgent::named("claude"),
            &crate::client::execution_ids::ExecutionId::from("execution"),
        )
        .unwrap();
        let mut subscription = subscribe(daemon.connector(), &[Topic::Workspaces]).await;
        snapshot(&mut subscription).await;
        daemon
            .manager
            .discard_stopped_work(&SpaceSelector::id(&workspace.id), None)
            .await
            .unwrap();
        until(
            &mut subscription,
            |update| matches!(update, Update::Changed(record) if matches!(&**record, Record::Workspace(w) if w.agent.is_none())),
        )
        .await;
    }

    #[tokio::test]
    async fn a_large_snapshot_arrives_in_pages() {
        let daemon = TestDaemon::start().await;
        for n in 0..200 {
            let message = format!("{n}: {}", "x".repeat(1000));
            daemon
                .manager
                .notify(None, NotificationKind::CleanupFailed, message)
                .await;
        }
        let stream = daemon.raw().await;
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        let subscribe = SubscriptionMethod::Subscribe {
            topics: vec![Topic::Notifications],
            log: None,
        };
        let request = Request {
            scope: None,
            ..Request::new(subscribe)
        };
        protocol::write(&mut writer, &request).await.unwrap();
        let mut frames = 0;
        let mut records = 0;
        loop {
            let response: protocol::Response = protocol::read_buffered(&mut reader).await.unwrap();
            let Body::Records(page) = response.body else {
                panic!("expected the snapshot: {response:?}");
            };
            frames += 1;
            records += page.len();
            if !response.more {
                break;
            }
        }
        assert!(frames > 1, "{frames} frames");
        assert_eq!(records, 200);
    }

    #[tokio::test]
    async fn a_daemon_quiescing_for_an_update_closes_a_subscription_instead_of_writing() {
        let daemon = TestDaemon::start().await;
        let workspace = daemon.workspace("kept").await;
        let stream = daemon.raw().await;
        let (reader, mut writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        let subscribe = SubscriptionMethod::Subscribe {
            topics: vec![Topic::Workspaces],
            log: None,
        };
        let request = Request {
            scope: None,
            ..Request::new(subscribe)
        };
        protocol::write(&mut writer, &request).await.unwrap();
        protocol::read_response(&mut reader).await.unwrap();
        let _quiesced = daemon.manager.gates.quiesce().await;
        daemon
            .manager
            .acquire_hold(
                &SpaceSelector::id(&workspace.id),
                "quay".into(),
                None,
                HoldPlacer::User,
            )
            .await
            .unwrap();
        let next = tokio::time::timeout(HANG, protocol::read_response(&mut reader))
            .await
            .unwrap();
        let error = next.unwrap_err();
        assert!(
            crate::client::connection::stream_closed(&error),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn after_a_restart_the_subscription_resyncs() {
        let mut daemon = TestDaemon::start().await;
        let workspace = daemon.workspace("kept").await;
        let mut subscription = subscribe(daemon.connector(), &[Topic::Workspaces]).await;
        snapshot(&mut subscription).await;
        daemon.restart().await;
        daemon
            .manager
            .acquire_hold(
                &SpaceSelector::id(&workspace.id),
                "quay".into(),
                None,
                HoldPlacer::User,
            )
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
        let Some(Record::Workspace(kept)) = records.first() else {
            panic!("expected the workspace: {records:?}");
        };
        assert_eq!(kept.workspace.holds.len(), 1);
    }
}
