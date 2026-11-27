use crate::daemon::conversions::relocation::fake::resumed;
use crate::daemon::conversions::relocation::*;

/// Notified once a relocation pass has listed the pending conversions.
use crate::{
    config::modes::ModeName,
    daemon::{
        conversions::Converting,
        manager::Manager,
        test_support::{ProcessGroup, manager, repository},
    },
    execution::recovery::{LastSession, LastSessionExt},
    model::Workspace,
    state::AgentState,
};
use shoal_protocol::space_names::SpaceSelector;

/// A chat of a new repository, whose agent `process` reported session
/// `s1`, converted into a workspace on `fix-login`, with its agent's
/// scope token and one the chat issued.
async fn converted(
    root: &std::path::Path,
    manager: &std::sync::Arc<Manager>,
) -> (
    Workspace,
    ProcessGroup,
    shoal_protocol::scope_tokens::ScopeToken,
    shoal_protocol::scope_tokens::ScopeToken,
) {
    let path = repository(root, "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let talk =
        crate::daemon::test_support::chat(manager, "talk", "worker", Some(&repo), None).await;
    let process = crate::daemon::test_support::agent_process(manager, &talk.id).await;
    let selector = SpaceSelector::id(&talk.id);
    let working = AgentState::Working;
    (manager
        .agents
        .set_agent_state(&**manager, &selector, working, None, None))
    .await
    .unwrap();
    crate::daemon::test_support::report_session(manager, &talk.id, "s1").await;
    let talk = manager.spaces.chat_by_id(&talk.id).await.unwrap().unwrap();
    let token = manager.spaces.chat_scope(&talk.id).await.unwrap();
    let issued = manager.spaces.issue_scope(&selector).await.unwrap();
    let converting = Converting {
        chat: talk,
        mode: ModeName::PLAN,
        carrying: Default::default(),
    };
    let workspace = manager
        .spaces
        .create_converted_workspace(
            &**manager,
            converting,
            "fix-login".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    (workspace, process, token, issued)
}

#[tokio::test]
async fn the_agent_restarts_in_the_worktree_once_its_turn_ends() {
    let (root, manager) = manager().await;
    let (workspace, mut process, token, issued) = converted(root.path(), &manager).await;
    let selector = SpaceSelector::id(&workspace.id);
    // The agent that converted its chat goes on in its turn, under its scope.
    assert!(
        manager
            .coordination
            .caller(&*manager, &token)
            .await
            .unwrap()
            .is_some()
    );

    let idle = AgentState::Idle;
    (manager
        .agents
        .set_agent_state(&*manager, &selector, idle, None, None))
    .await
    .unwrap();
    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    process.wait().unwrap();
    assert_eq!(resumed(&manager.paths), std::slice::from_ref(&workspace.id));
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    let [last] = sessions.as_slice() else {
        panic!("one last session to continue: {sessions:?}");
    };
    assert!(last.converted);
    assert_eq!(
        last.session.as_ref().map(ToString::to_string),
        Some("s1".into())
    );
    assert_eq!(last.mode, ModeName::PLAN);
    // Neither the agent's token nor one the chat issued acts for the
    // workspace it became.
    assert!(
        manager
            .coordination
            .caller(&*manager, &token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        manager
            .coordination
            .caller(&*manager, &issued)
            .await
            .unwrap()
            .is_none()
    );
    let id = workspace.id.clone();
    let pending = (manager.store)
        .run(move |db| crate::daemon::conversions::pending(db, &id))
        .await
        .unwrap();
    assert!(pending.is_none());
}

#[tokio::test]
async fn a_converted_chats_token_acts_for_the_workspace_it_became() {
    let (root, manager) = manager().await;
    let (workspace, _process, token, _issued) = converted(root.path(), &manager).await;
    // As after a restart, before the agent's wrapper connects again.
    manager.agents.executions().scopes().await.clear();

    let caller = manager
        .coordination
        .caller(&*manager, &token)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(caller.space_id, workspace.id);
}

#[tokio::test]
async fn a_working_agent_finishes_its_turn_where_it_runs() {
    let (root, manager) = manager().await;
    let (workspace, _process, token, _issued) = converted(root.path(), &manager).await;

    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    assert!(resumed(&manager.paths).is_empty());
    assert!(
        manager
            .coordination
            .caller(&*manager, &token)
            .await
            .unwrap()
            .is_some()
    );
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    assert!(sessions.is_empty(), "{sessions:?}");
}

#[tokio::test]
async fn an_agent_waits_where_it_runs_while_the_workspaces_setup_failed() {
    let (root, manager) = manager().await;
    let (workspace, _process, token, _issued) = converted(root.path(), &manager).await;
    let id = workspace.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute("UPDATE workspaces SET state='failed' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    (manager
        .agents
        .set_agent_state(&*manager, &selector, AgentState::Idle, None, None))
    .await
    .unwrap();

    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    assert!(resumed(&manager.paths).is_empty());
    assert!(
        manager
            .coordination
            .caller(&*manager, &token)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_stop_of_the_workspace_keeps_the_converting_agents_session() {
    let (root, manager) = manager().await;
    let (workspace, mut process, token, _issued) = converted(root.path(), &manager).await;
    let targets = (manager
        .agents
        .stop_targets(&*manager, crate::protocol::QueuedLaunches::Keep))
    .await
    .unwrap();
    assert!(targets.iter().any(|target| target.id == workspace.id));

    let selector = SpaceSelector::id(&workspace.id);
    let keep = crate::protocol::QueuedLaunches::Keep;
    (manager
        .agents
        .stop_space_manually(&*manager, &selector, false, keep))
    .await
    .unwrap();

    process.wait().unwrap();
    assert!(resumed(&manager.paths).is_empty());
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    let [last] = sessions.as_slice() else {
        panic!("one last session to continue: {sessions:?}");
    };
    assert!(last.converted);
    assert!(
        manager
            .coordination
            .caller(&*manager, &token)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_stop_while_the_workspace_is_set_up_stops_the_converting_agent() {
    let (root, manager) = manager().await;
    let (workspace, mut process, _token, _issued) = converted(root.path(), &manager).await;
    let id = workspace.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute("UPDATE workspaces SET state='preparing' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    let targets = (manager
        .agents
        .stop_targets(&*manager, crate::protocol::QueuedLaunches::Keep))
    .await
    .unwrap();
    assert!(targets.iter().any(|target| target.id == workspace.id));

    let selector = SpaceSelector::id(&workspace.id);
    let keep = crate::protocol::QueuedLaunches::Keep;
    (manager
        .agents
        .stop_space_manually(&*manager, &selector, false, keep))
    .await
    .unwrap();

    process.wait().unwrap();
    let state = manager
        .spaces
        .workspace_by_id(&workspace.id)
        .await
        .unwrap()
        .state;
    assert_eq!(state, crate::state::WorkspaceState::Preparing);
}

#[tokio::test]
async fn the_relocation_restarts_no_agent_a_stop_handed_over() {
    let (root, manager) = manager().await;
    let (workspace, _process, _token, _issued) = converted(root.path(), &manager).await;
    let selector = SpaceSelector::id(&workspace.id);
    (manager
        .agents
        .set_agent_state(&*manager, &selector, AgentState::Idle, None, None))
    .await
    .unwrap();
    let keep = crate::protocol::QueuedLaunches::Keep;
    (manager
        .agents
        .stop_space_manually(&*manager, &selector, false, keep))
    .await
    .unwrap();

    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    assert!(resumed(&manager.paths).is_empty());
}

#[tokio::test]
async fn a_workspace_whose_agent_exited_still_becomes_a_chat_with_its_session() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let execution = shoal_ids::execution_ids::ExecutionId::generate();
    // The agent exited on its own after the conversion was asked for,
    // keeping its session as a last session.
    let exited = LastSession {
        turn: None,
        execution: execution.clone(),
        agent: "claude".into(),
        session: Some("s2".into()),
        moved: false,
        copied: None,
        converted: false,
        handoff: None,
        origin: None,
        stop: None,
        model: Default::default(),
        mode: ModeName::DEFAULT,
        narrowing: Default::default(),
    };
    exited.write(&manager.paths, &workspace.id).unwrap();
    let chat = crate::daemon::conversions::pending::ConvertedChat {
        agent: "claude".into(),
        execution,
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    record_chat_conversion(&manager, &workspace, chat).await;

    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    let chat = manager
        .spaces
        .chat_by_id(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    let last = LastSession::all(&manager.paths, &chat.id).unwrap();
    assert!(last.iter().all(|last| last.converted), "{last:?}");
    let sessions = manager.spaces.reported_sessions(&chat.id).await.unwrap();
    let sessions: Vec<_> = (sessions.iter())
        .map(|reported| reported.session.as_str())
        .collect();
    assert_eq!(sessions, ["s2"]);
}

/// Whether a restore of the agent of the space `id` is queued.
async fn restore_queued(manager: &Manager, id: &shoal_ids::space_ids::SpaceId) -> bool {
    let entries = manager.agents.queue_entries(manager).await.unwrap();
    entries
        .iter()
        .any(|entry| entry.space_id.as_ref() == Some(id))
}

/// Record that `workspace` becomes `chat` once its agent's turn ends.
async fn record_chat_conversion(
    manager: &Manager,
    workspace: &Workspace,
    chat: crate::daemon::conversions::pending::ConvertedChat,
) {
    let (id, pending) = (
        workspace.id.clone(),
        crate::daemon::conversions::Pending::Chat(crate::daemon::conversions::pending::ToChat {
            chat: Box::new(chat),
            held: false,
            carrying: Default::default(),
        }),
    );
    (manager.store)
        .write(move |tx| crate::daemon::conversions::pending::record(tx, &id, &pending))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_stop_keeps_the_chat_a_workspace_becomes_down() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let chat = crate::daemon::conversions::pending::ConvertedChat {
        agent: "claude".into(),
        execution: shoal_ids::execution_ids::ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    record_chat_conversion(&manager, &workspace, chat).await;
    let selector = SpaceSelector::id(&workspace.id);
    let keep = crate::protocol::QueuedLaunches::Keep;
    (manager
        .agents
        .stop_space_manually(&*manager, &selector, false, keep))
    .await
    .unwrap();

    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    let chat = manager
        .spaces
        .chat_by_id(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!chat.running);
    assert!(!restore_queued(&manager, &workspace.id).await);
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
}

#[tokio::test]
async fn a_stop_while_the_relocation_waits_keeps_the_chat_down() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let chat = crate::daemon::conversions::pending::ConvertedChat {
        agent: "claude".into(),
        execution: shoal_ids::execution_ids::ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    record_chat_conversion(&manager, &workspace, chat).await;
    // A stop holds the hand-overs until released.
    let (held, holding) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let stop = tokio::spawn({
        let manager = manager.clone();
        async move {
            let stopping = async {
                held.send(()).unwrap();
                released.await.ok();
            };
            manager.spaces.conversion_handovers.hold(stopping).await;
        }
    });
    holding.await.unwrap();
    let relocating = tokio::spawn({
        let manager = manager.clone();
        async move { manager.spaces.relocate_finished_turns(&*manager).await }
    });
    // The relocation lists the conversion, then waits for the hand-overs,
    // which a stop holds while it holds the conversion.
    LISTED.notified().await;
    let id = workspace.id.clone();
    (manager.store)
        .run(move |db| crate::daemon::conversions::pending::hold(db, &id))
        .await
        .unwrap();
    release.send(()).unwrap();
    stop.await.unwrap();

    relocating.await.unwrap().unwrap();

    let chat = manager
        .spaces
        .chat_by_id(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!chat.running);
    assert!(!restore_queued(&manager, &workspace.id).await);
}

#[tokio::test]
async fn the_hand_over_continues_the_session_reported_after_the_conversion() {
    let (root, manager) = manager().await;
    let (workspace, mut process, _token, _issued) = converted(root.path(), &manager).await;
    let selector = SpaceSelector::id(&workspace.id);
    let idle = AgentState::Idle;

    crate::daemon::test_support::report_session(&manager, &workspace.id, "s3").await;
    (manager
        .agents
        .set_agent_state(&*manager, &selector, idle, None, None))
    .await
    .unwrap();
    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    process.wait().unwrap();
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    let reported: Vec<_> = (sessions.iter())
        .map(|last| last.session.as_ref().map(ToString::to_string))
        .collect();
    assert_eq!(reported, [Some("s3".to_owned())]);
}

#[tokio::test]
async fn a_restart_that_exits_before_reporting_a_session_keeps_the_converted_one() {
    let (root, manager) = manager().await;
    let (workspace, mut process, _token, _issued) = converted(root.path(), &manager).await;
    let selector = SpaceSelector::id(&workspace.id);
    let idle = AgentState::Idle;
    (manager
        .agents
        .set_agent_state(&*manager, &selector, idle, None, None))
    .await
    .unwrap();
    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();
    process.wait().unwrap();
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    let [handed] = sessions.as_slice() else {
        panic!("one last session to continue: {sessions:?}");
    };
    let record = LastSession::path(&manager.paths, &workspace.id, &handed.execution);

    exit_before_reporting(&manager, &workspace.id, record).await;

    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    let [last] = sessions.as_slice() else {
        panic!("one last session to continue: {sessions:?}");
    };
    assert_ne!(last.execution, handed.execution);
    assert_eq!(
        last.session.as_ref().map(ToString::to_string),
        Some("s1".into())
    );
    assert!(last.converted);
}

/// Run, as `shoal resume` would, the restore of the agent of the space `id`
/// saved at `record`, whose agent exits with 1 before its hooks report a
/// session.
async fn exit_before_reporting(
    manager: &std::sync::Arc<Manager>,
    id: &shoal_ids::space_ids::SpaceId,
    record: std::path::PathBuf,
) {
    use crate::daemon::execution_connection::{ExecutionContext, execute};
    use crate::protocol::{self, Control, ExecutionEvent, Response};
    use tokio::io::BufReader;

    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let agent = shoal_protocol::executions::TrackedAgent {
        mode: ModeName::PLAN,
        ..shoal_protocol::executions::TrackedAgent::named("claude")
    };
    let executing = tokio::spawn(execute(
        server,
        1,
        manager.agents.clone(),
        manager.clone(),
        ExecutionContext {
            space: SpaceSelector::id(id),
            wrapper: wrapper.clone(),
            kind: crate::daemon::executions::ExecutionKind::Command,
            agent: Some(agent),
            recover: false,
            reattach: true,
            caller: None,
            queue_entry: None,
            demo: None,
            origin: None,
            merge: None,
        },
        manager.gates.owned_operation().await,
    ));
    let mut wrapper_side = BufReader::new(client);
    let _: Response = protocol::read_buffered(&mut wrapper_side).await.unwrap();
    let started = ExecutionEvent::Started {
        child: None,
        group_id: wrapper.pid,
        restored: true,
        restored_from: vec![record.clone()],
        reports_status: false,
    };
    protocol::write(wrapper_side.get_mut(), &started)
        .await
        .unwrap();
    let control: Control = protocol::read_buffered(&mut wrapper_side).await.unwrap();
    assert!(matches!(control, Control::Started), "{control:?}");
    // The wrapper consumes the record it restores once its start is
    // acknowledged.
    crate::execution::recovery::consume(&record).unwrap();
    let finished = ExecutionEvent::Finished {
        exit_code: 1,
        memory: None,
    };
    protocol::write(wrapper_side.get_mut(), &finished)
        .await
        .unwrap();
    loop {
        match protocol::read_buffered(&mut wrapper_side).await.unwrap() {
            Control::Finished { .. } => break,
            Control::ProgramStatus { .. } => {}
            control => panic!("expected the execution to finish, got {control:?}"),
        }
    }
    executing.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_relocation_hands_over_the_converting_agents_its_removal_reaches() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "base".into(), None, None, None)
        .await
        .unwrap();
    // A chat in its worktree, below which a chat became a workspace whose
    // agent has yet to restart there.
    let inside = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "inside".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let below = crate::daemon::test_support::chat(
        &manager,
        "below",
        "worker",
        Some(&repo),
        Some(&inside.id),
    )
    .await;
    let mut below_process = crate::daemon::test_support::agent_process(&manager, &below.id).await;
    let converting = Converting {
        chat: below,
        mode: ModeName::DEFAULT,
        carrying: Default::default(),
    };
    let converted = manager
        .spaces
        .create_converted_workspace(
            &*manager,
            converting,
            "part".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let chat = crate::daemon::conversions::pending::ConvertedChat {
        agent: "claude".into(),
        execution: shoal_ids::execution_ids::ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    record_chat_conversion(&manager, &workspace, chat).await;

    let relocated = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        manager.spaces.relocate_finished_turns(&*manager),
    )
    .await
    .expect("the relocation's removal takes the hand-overs it holds");

    relocated.unwrap();
    below_process.wait().unwrap();
    assert!(
        manager
            .spaces
            .chat_by_id(&workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(manager.spaces.workspace_by_id(&converted.id).await.is_err());
}

/// Start the preparation of `workspace` as a conversion's, with a worker the
/// test drives, and record it with the conversion.
async fn record_preparation(
    manager: &Manager,
    workspace: &Workspace,
) -> (String, crate::daemon::operations::fake::Fake) {
    use crate::protocol::operations::{OperationSpec, Preparation};
    let spec = OperationSpec::Prepare {
        workspace: SpaceSelector::id(&workspace.id),
        steps: Preparation {
            post_setup: true,
            ..Default::default()
        },
    };
    let operation = manager
        .spaces
        .start_operation(spec, None, None)
        .await
        .unwrap();
    let (id, recorded) = (workspace.id.clone(), operation.clone());
    (manager.store)
        .run(move |db| crate::daemon::conversions::pending::record_preparation(db, &id, &recorded))
        .await
        .unwrap();
    let worker = crate::daemon::operations::fake::take(&operation);
    (operation, worker)
}

#[tokio::test]
async fn the_agent_waits_for_its_workspaces_post_setup_hook() {
    let (root, manager) = manager().await;
    let (workspace, _process, token, _issued) = converted(root.path(), &manager).await;
    let (_operation, _worker) = record_preparation(&manager, &workspace).await;
    let selector = SpaceSelector::id(&workspace.id);
    (manager
        .agents
        .set_agent_state(&*manager, &selector, AgentState::Idle, None, None))
    .await
    .unwrap();

    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    assert!(resumed(&manager.paths).is_empty());
    assert!(
        manager
            .coordination
            .caller(&*manager, &token)
            .await
            .unwrap()
            .is_some()
    );
    assert!(manager.spaces.converting(&workspace.id).await.unwrap());
}

#[tokio::test]
async fn a_failed_post_setup_hook_stops_the_agent_for_shoal_resume() {
    use crate::protocol::operations::{Ended, Failure, Report};
    let (root, manager) = manager().await;
    let (workspace, mut process, token, _issued) = converted(root.path(), &manager).await;
    let (operation, worker) = record_preparation(&manager, &workspace).await;
    let failed = Ended::Failed(Failure::HookFailed {
        message: "hook exited 1".into(),
    });
    crate::daemon::requests::operations::entries::report(
        &manager,
        &operation,
        Report::Ended(failed),
        None,
    )
    .await
    .unwrap();
    worker.exit(1);
    let entry = crate::daemon::requests::operations::entries::operation(&manager, &operation, None)
        .await
        .unwrap();
    let mut changed = entry.watch();
    while entry.events_from(0).1.is_none() {
        changed.changed().await.unwrap();
    }
    let selector = SpaceSelector::id(&workspace.id);
    (manager
        .agents
        .set_agent_state(&*manager, &selector, AgentState::Idle, None, None))
    .await
    .unwrap();

    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    // Stopped with its session kept for shoal resume, not restarted.
    process.wait().unwrap();
    assert!(resumed(&manager.paths).is_empty());
    assert!(
        manager
            .coordination
            .caller(&*manager, &token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!manager.spaces.converting(&workspace.id).await.unwrap());
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
}

#[tokio::test]
async fn a_failed_hook_outlasts_its_operations_record() {
    let (root, manager) = manager().await;
    let (workspace, mut process, _token, _issued) = converted(root.path(), &manager).await;
    // An operation the daemon no longer follows, as after its retention or
    // a restart, whose end the conversion kept.
    let id = workspace.id.clone();
    (manager.store)
        .run(move |db| {
            crate::daemon::conversions::pending::record_preparation(db, &id, "gone")?;
            crate::daemon::conversions::pending::record_prepared(
                db,
                &id,
                "gone",
                Some("hook exited 1"),
            )
        })
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    (manager
        .agents
        .set_agent_state(&*manager, &selector, AgentState::Idle, None, None))
    .await
    .unwrap();

    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    process.wait().unwrap();
    assert!(resumed(&manager.paths).is_empty());
    assert!(!manager.spaces.converting(&workspace.id).await.unwrap());
}

#[tokio::test]
async fn a_preparation_end_reported_before_its_record_is_kept() {
    let (root, manager) = manager().await;
    let (workspace, _process, _token, _issued) = converted(root.path(), &manager).await;
    let id = workspace.id.clone();

    let pending = (manager.store)
        .run(move |db| {
            use crate::daemon::conversions::pending;
            pending::record_prepared(db, &id, "early", Some("hook exited 1"))?;
            pending::record_preparation(db, &id, "early")?;
            crate::daemon::conversions::pending(db, &id)
        })
        .await
        .unwrap();

    let Some(crate::daemon::conversions::Pending::Workspace(
        crate::daemon::conversions::pending::ToWorkspace {
            prepared,
            hook_failure,
            ..
        },
    )) = pending
    else {
        panic!("expected the conversion: {pending:?}");
    };
    assert!(prepared);
    assert_eq!(hook_failure.as_deref(), Some("hook exited 1"));
}
