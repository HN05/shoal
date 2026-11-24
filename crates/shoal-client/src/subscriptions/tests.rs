//! Subscriptions an app opens, as the user and as a workspace's agent, to a
//! daemon on an isolated state directory.
use crate::connection::Connector;
use crate::forge::items::link::LinkKind;
use crate::test_daemon::Connect as _;
use shoal_core::execution::recovery::LastSessionExt as _;
use shoal_core::removal::{InspectionPolicy, UnsavedWork};
use shoal_daemon::test_daemon::Access;
use shoal_daemon::test_daemon::{
    ApprovalRequestExt as _, HANG, TestDaemon, link_item, link_space, store_issue_proposal,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::alerts::Sender;
use shoal_protocol::config::repo::Lifetime;
use shoal_protocol::model::{HoldPlacer, Relation};
use shoal_protocol::protocol::{self, ErrorCode, RemoteError, Request, SubscriptionMethod};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::subscriptions::{
    Update,
    records::{Record, RecordKind, Topic},
};
use shoal_protocol::threads::ThreadOpening;
use shoal_protocol::threads::{NewPost, OutcomeKind, Resolving, Thread, ThreadKind, ThreadNumber};

use super::Subscription;

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

/// A process of the space `id`.
fn caller(id: &SpaceId) -> shoal_daemon::test_daemon::Caller {
    shoal_daemon::test_daemon::Caller {
        space_id: id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    }
}

/// Have the space `id` ask its coordinator, else the user, in a thread.
async fn ask(manager: &shoal_daemon::test_daemon::Manager, id: &SpaceId) -> Thread {
    let caller = caller(id);
    let name = "Which database?".to_owned();
    let thread = (manager.coordination.open_thread(
        manager,
        Some(&caller),
        ThreadOpening::named(name),
        Some(ThreadKind::Question),
        None,
    ))
    .await
    .unwrap();
    let post = NewPost {
        text: "Postgres or SQLite?".into(),
        ..NewPost::default()
    };
    (manager.coordination.post_to_thread(
        manager,
        Some(&caller),
        ThreadNumber::Full(thread.number),
        post,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
    .await
    .unwrap()
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
    assert!(kinds.contains(&RecordKind::Space), "{kinds:?}");

    let manager = &daemon.manager;
    let other = shoal_core::test_support::repository(&manager.paths.home, "other");
    manager
        .repositories
        .register(other.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    until(&mut subscription, changed(RecordKind::Repository)).await;

    manager
        .spaces
        .acquire_hold(
            &**manager,
            &SpaceSelector::id(&workspace.id),
            "quay".into(),
            None,
            HoldPlacer::User,
        )
        .await
        .unwrap();
    until(&mut subscription, |update| {
        matches!(update, Update::Changed(record) if matches!(&**record, Record::Space(s) if s.space.holds.len() == 1))
    })
    .await;

    link_item(
        manager,
        &SpaceSelector::id(&workspace.id),
        LinkKind::Issue,
        &"40".parse().unwrap(),
        Relation::Related,
        Some("Report".into()),
    )
    .await
    .unwrap();
    until(&mut subscription, changed(RecordKind::Link)).await;

    let question = ask(manager, &workspace.id).await;
    until_each(&mut subscription, &[RecordKind::Thread, RecordKind::Alert]).await;

    manager
        .coordination
        .send_space_message(
            &**manager,
            None,
            &SpaceSelector::id(&workspace.id),
            "Rebase on main".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    until(&mut subscription, |update| {
        matches!(update, Update::Changed(record) if matches!(&**record, Record::Alert(m) if m.recipient.is_some()))
    })
    .await;
    manager
        .coordination
        .send_alert(
            &**manager,
            Some(&shoal_daemon::test_daemon::Caller {
                space_id: workspace.id.clone(),
                execution: None,
                granted: None,
                access: Access::Full,
                vouched: None,
            }),
            shoal_protocol::protocol::AlertTarget::Up,
            "Ready for review".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    until(&mut subscription, |update| {
        matches!(update, Update::Changed(record) if matches!(&**record, Record::Alert(m) if m.recipient.is_none() && m.sender == Sender::Space))
    })
    .await;

    let answered = Resolving {
        outcome: Some(OutcomeKind::Answered),
        ..Resolving::default()
    };
    manager
        .coordination
        .resolve_thread(
            &**manager,
            Some(&caller(&workspace.id)),
            ThreadNumber::Full(question.number),
            answered,
        )
        .await
        .unwrap();
    until(
        &mut subscription,
        |update| matches!(update, Update::Removed(key) if key.kind == RecordKind::Thread),
    )
    .await;

    manager
        .spaces
        .remove_selected(
            &**manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    until(&mut subscription, |update| {
        matches!(update, Update::Removed(key) if key.kind == RecordKind::Space && key.id == workspace.id)
    })
    .await;
}

#[tokio::test]
async fn an_approval_request_reaches_a_subscription() {
    use shoal_core::config::repo::ConflictPolicy;
    use shoal_protocol::approvals::{ApprovalRequest, PortSpecification, Specification, Target};
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
    daemon
        .manager
        .coordination
        .request_approval(request)
        .await
        .unwrap();
    until(&mut subscription, changed(RecordKind::Approval)).await;
}

#[tokio::test]
async fn the_user_keeps_seeing_an_issue_proposal_its_workspace_left() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("proposer").await;
    let proposal = store_issue_proposal(&daemon.manager, &workspace.clone().into()).await;
    daemon
        .manager
        .spaces
        .remove_selected(
            &*daemon.manager,
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
    let daemon = TestDaemon::start().await;
    let chat = daemon
        .manager
        .start_chat(
            daemon.repository.id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let other = daemon.workspace("other").await;
    store_issue_proposal(&daemon.manager, &other.into()).await;
    let token = daemon.manager.spaces.chat_scope(&chat.id).await.unwrap();
    let connector = daemon.connector().scope(token);
    let mut subscription = subscribe(connector, &[Topic::Approvals]).await;
    let records = snapshot(&mut subscription).await;
    assert!(records.is_empty(), "{records:?}");
    let space = link_space(&daemon.manager, &SpaceSelector::id(&chat.id))
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
async fn the_user_sees_the_threads_of_a_chat() {
    let daemon = TestDaemon::start().await;
    let chat = daemon
        .manager
        .start_chat(
            daemon.repository.id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let question = ask(&daemon.manager, &chat.id).await;
    let mut subscription = subscribe(daemon.connector(), &[Topic::Threads]).await;
    let records = snapshot(&mut subscription).await;
    let [Record::Thread(shown)] = records.as_slice() else {
        panic!("expected the chat's thread: {records:?}");
    };
    assert_eq!((shown.number, &*shown.space), (question.number, "planning"));
}

#[tokio::test]
async fn a_chat_reaches_a_subscription_with_its_agent_links_and_messages() {
    use shoal_protocol::model::SpaceKind;
    let daemon = TestDaemon::start().await;
    let manager = &daemon.manager;
    let chat = manager
        .start_chat(
            daemon.repository.id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let selector = SpaceSelector::id(&chat.id);
    link_item(
        manager,
        &selector,
        LinkKind::Issue,
        &"40".parse().unwrap(),
        Relation::Related,
        Some("Report".into()),
    )
    .await
    .unwrap();
    manager
        .coordination
        .send_space_message(&**manager, None, &selector, "Plan it".into(), Vec::new())
        .await
        .unwrap();
    let topics = [Topic::Spaces, Topic::Links, Topic::Alerts];
    let mut subscription = subscribe(daemon.connector(), &topics).await;
    let records = snapshot(&mut subscription).await;
    let [
        Record::Space(space),
        Record::Link(link),
        Record::Alert(message),
    ] = records.as_slice()
    else {
        panic!("expected the chat, its link and its message: {records:?}");
    };
    assert_eq!(
        (&space.space.id, space.space.kind()),
        (&chat.id, SpaceKind::Chat)
    );
    let agent = space.agent.as_ref().and_then(|agent| agent.agent.as_ref());
    assert_eq!(agent.map(AsRef::<str>::as_ref), Some("claude"));
    assert_eq!(link.space_id, chat.id);
    let recipient = message.recipient.as_ref();
    assert_eq!(
        recipient.map(|to| (&to.id, to.name.as_str())),
        Some((&chat.id, "planning"))
    );
}

#[tokio::test]
async fn a_session_an_agent_reports_reaches_a_subscription() {
    use shoal_protocol::executions::TrackedAgent;
    use shoal_protocol::transcripts::ReportedSession;
    use shoal_protocol::{executions::ExecutionKind, state::AgentState};
    let daemon = TestDaemon::start().await;
    let manager = &daemon.manager;
    let workspace = daemon.workspace("work").await;
    let chat = manager
        .start_chat(
            daemon.repository.id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
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
        .agents
        .begin_execution(
            &**manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap()
        .plan
        .id;
    manager
        .agents
        .track_agent(
            &**manager,
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
            .agents
            .set_agent_state(
                &**manager,
                &SpaceSelector::id(&workspace.id),
                AgentState::Idle,
                Some(execution.clone()),
                Some(session.into()),
            )
            .await
            .unwrap();
        until(&mut subscription, reported(workspace.id.clone(), session)).await;
    }

    let execution = manager.spaces.workspace_executions(&chat.id).await.unwrap()[0]
        .id
        .clone();
    manager
        .agents
        .set_agent_state(
            &**manager,
            &SpaceSelector::id(&chat.id),
            AgentState::Idle,
            Some(execution),
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
    ask(&daemon.manager, &other.id).await;
    let elsewhere = shoal_core::test_support::repository(&daemon.manager.paths.home, "elsewhere");
    daemon
        .manager
        .repositories
        .register(elsewhere.to_str().unwrap().into(), None, None)
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
    assert_eq!(ids(RecordKind::Space), [own.id.as_str()]);
    assert_eq!(ids(RecordKind::Repository), [daemon.repository.id.as_str()]);
    assert!(
        keys.iter()
            .all(|key| !matches!(key.kind, RecordKind::Alert | RecordKind::Thread)),
        "{keys:?}"
    );
}

#[tokio::test]
async fn a_log_subscription_follows_one_space_within_its_scope() {
    use shoal_protocol::log::{LogEntry, LogFilter, LogTopic};
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    let other = daemon.workspace("other").await;
    let topic = |space: &SpaceId| LogTopic {
        space: Some(SpaceSelector::id(space)),
        filter: LogFilter::Entries,
        limit: 50,
    };
    let refused = Subscription::follow_log(daemon.scoped(&own).await, topic(&other.id)).await;
    assert!(refused.is_err());
    let mut subscription = Subscription::follow_log(daemon.scoped(&own).await, topic(&own.id))
        .await
        .unwrap();
    let records = snapshot(&mut subscription).await;
    assert!(records.iter().all(|record| matches!(
        record,
        Record::LogEntry(entry) if matches!(&**entry, LogEntry::Event(event)
            if event.details.space_id == own.id)
    )));
    assert!(!records.is_empty());
    daemon
        .manager
        .spaces
        .rename_space(&SpaceSelector::id(&own.id), &"Login fix".into())
        .await
        .unwrap();
    // Another space's entries stay out.
    daemon
        .manager
        .spaces
        .rename_space(&SpaceSelector::id(&other.id), &"Other".into())
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
    assert_eq!(event.details.name, "Login fix");
}

#[tokio::test]
async fn a_log_subscription_keeps_following_its_space_after_a_rename() {
    use shoal_protocol::log::{LogEntry, LogFilter, LogTopic};
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    let topic = LogTopic {
        space: Some(SpaceSelector::id(&own.id)),
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
            db.execute("UPDATE spaces SET name='renamed' WHERE id=?1", [&id])?;
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
async fn a_scoped_subscription_without_space_records_names_its_tree() {
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    let other = daemon.workspace("other").await;
    for workspace in [&own, &other] {
        ask(&daemon.manager, &workspace.id).await;
        daemon
            .manager
            .coordination
            .send_space_message(
                &*daemon.manager,
                None,
                &SpaceSelector::id(&workspace.id),
                "Rebase on main".into(),
                Vec::new(),
            )
            .await
            .unwrap();
    }
    let topics = [Topic::Threads, Topic::Alerts];
    let mut subscription = subscribe(daemon.scoped(&own).await, &topics).await;
    let records = snapshot(&mut subscription).await;
    assert_eq!(records.len(), 2, "{records:?}");
    for record in &records {
        match record {
            Record::Thread(thread) => assert_eq!(thread.space_id, own.id),
            Record::Alert(message) => {
                let recipient = message.recipient.as_ref();
                assert_eq!(
                    recipient.map(|to| (&to.id, to.name.as_str())),
                    Some((&own.id, own.name.as_str()))
                );
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
    let mut subscription = subscribe(connector(), &[Topic::Spaces]).await;
    snapshot(&mut subscription).await;
    daemon
        .manager
        .spaces
        .revoke_scope(&SpaceSelector::id(&workspace.id), token.clone())
        .await
        .unwrap();
    let refused = Subscription::open(connector(), vec![Topic::Spaces]).await;
    assert_eq!(code(refused.err().unwrap()), ErrorCode::ScopeDenied);
    daemon
        .manager
        .spaces
        .acquire_hold(
            &*daemon.manager,
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
    shoal_core::execution::recovery::LastSession::save(
        &daemon.manager.paths,
        &workspace.id,
        &shoal_protocol::executions::TrackedAgent::named("claude"),
        &shoal_ids::execution_ids::ExecutionId::from("execution"),
        None,
    )
    .unwrap();
    let mut subscription = subscribe(daemon.connector(), &[Topic::Spaces]).await;
    snapshot(&mut subscription).await;
    daemon
        .manager
        .agents
        .discard_stopped_agent(&*daemon.manager, &SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    until(
        &mut subscription,
        |update| matches!(update, Update::Changed(record) if matches!(&**record, Record::Space(s) if s.agent.is_none())),
    )
    .await;
}

#[tokio::test]
async fn a_daemon_quiescing_for_an_update_closes_a_subscription_instead_of_writing() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("kept").await;
    let stream = daemon.raw().await;
    let (reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    let subscribe = SubscriptionMethod::Subscribe {
        topics: vec![Topic::Spaces],
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
        .spaces
        .acquire_hold(
            &*daemon.manager,
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
    assert!(crate::connection::stream_closed(&error), "{error:#}");
}

#[tokio::test]
async fn after_a_restart_the_subscription_resyncs() {
    let mut daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("kept").await;
    let mut subscription = subscribe(daemon.connector(), &[Topic::Spaces]).await;
    snapshot(&mut subscription).await;
    daemon.restart().await;
    daemon
        .manager
        .spaces
        .acquire_hold(
            &*daemon.manager,
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
    let Some(Record::Space(kept)) = records.first() else {
        panic!("expected the workspace: {records:?}");
    };
    assert_eq!(kept.space.holds.len(), 1);
}
