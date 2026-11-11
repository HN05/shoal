use crate::client::space_ids::SpaceId;
use std::sync::Mutex;

/// The workspaces whose last session a relocation continued.
pub(crate) static RESUMED: Mutex<Vec<SpaceId>> = Mutex::new(Vec::new());

/// Notified once a relocation pass has listed the pending conversions.
pub(crate) static LISTED: tokio::sync::Notify = tokio::sync::Notify::const_new();

use crate::client::space_names::SpaceSelector;
use crate::{
    config::modes::ModeName,
    daemon::{
        conversions::Converting,
        test_support::{ProcessGroup, manager, repository},
        workspace::Manager,
    },
    execution::recovery::{LastSession, LastSessionExt},
    model::Workspace,
    state::AgentState,
};

/// A chat of a new repository, whose agent `process` reported session
/// `s1`, converted into a workspace on `fix-login`.
async fn converted(
    root: &std::path::Path,
    manager: &Manager,
) -> (
    Workspace,
    ProcessGroup,
    crate::client::scope_tokens::ScopeToken,
) {
    let path = repository(root, "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (talk, process) =
        crate::daemon::test_support::chat(manager, "talk", "worker", Some(&repo), None).await;
    let selector = SpaceSelector::id(&talk.id);
    let working = AgentState::Working;
    (manager.set_agent_state(&selector, working, None, Some("s1".into())))
        .await
        .unwrap();
    let talk = manager.chat_by_id(&talk.id).await.unwrap().unwrap();
    let token = manager.chat_scope(&talk.id).await.unwrap();
    let converting = Converting {
        chat: talk,
        mode: ModeName::PLAN,
    };
    let workspace = manager
        .create_converted_workspace(converting, "fix-login".into(), None, Default::default())
        .await
        .unwrap();
    (workspace, process, token)
}

#[tokio::test]
async fn the_agent_restarts_in_the_worktree_once_its_turn_ends() {
    let (root, manager) = manager().await;
    let (workspace, mut process, token) = converted(root.path(), &manager).await;
    let selector = SpaceSelector::id(&workspace.id);
    // The agent that converted its chat goes on in its turn, under its scope.
    assert!(manager.caller(&token).await.unwrap().is_some());

    let idle = AgentState::Idle;
    (manager.set_agent_state(&selector, idle, None, None))
        .await
        .unwrap();
    manager.relocate_finished_turns().await.unwrap();

    process.wait().unwrap();
    assert_eq!(
        *RESUMED.lock().unwrap(),
        std::slice::from_ref(&workspace.id)
    );
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
    assert!(manager.caller(&token).await.unwrap().is_none());
    let id = workspace.id.clone();
    let pending = (manager.store)
        .run(move |db| crate::daemon::conversions::pending(db, &id))
        .await
        .unwrap();
    assert!(pending.is_none());
}

#[tokio::test]
async fn a_working_agent_finishes_its_turn_where_it_runs() {
    let (root, manager) = manager().await;
    let (workspace, _process, token) = converted(root.path(), &manager).await;

    manager.relocate_finished_turns().await.unwrap();

    assert!(RESUMED.lock().unwrap().is_empty());
    assert!(manager.caller(&token).await.unwrap().is_some());
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    assert!(sessions.is_empty(), "{sessions:?}");
}

#[tokio::test]
async fn an_agent_waits_where_it_runs_while_the_workspaces_setup_failed() {
    let (root, manager) = manager().await;
    let (workspace, _process, token) = converted(root.path(), &manager).await;
    let id = workspace.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute("UPDATE workspaces SET state='failed' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    (manager.set_agent_state(&selector, AgentState::Idle, None, None))
        .await
        .unwrap();

    manager.relocate_finished_turns().await.unwrap();

    assert!(RESUMED.lock().unwrap().is_empty());
    assert!(manager.caller(&token).await.unwrap().is_some());
}

#[tokio::test]
async fn a_stop_of_the_workspace_keeps_the_converting_agents_session() {
    let (root, manager) = manager().await;
    let (workspace, mut process, token) = converted(root.path(), &manager).await;
    let targets = (manager.stop_targets(crate::protocol::QueuedLaunches::Keep))
        .await
        .unwrap();
    assert!(targets.iter().any(|target| target.id == workspace.id));

    let selector = SpaceSelector::id(&workspace.id);
    let keep = crate::protocol::QueuedLaunches::Keep;
    (manager.stop_workspace_manually(&selector, false, None, keep))
        .await
        .unwrap();

    process.wait().unwrap();
    assert!(RESUMED.lock().unwrap().is_empty());
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    let [last] = sessions.as_slice() else {
        panic!("one last session to continue: {sessions:?}");
    };
    assert!(last.converted);
    assert!(manager.caller(&token).await.unwrap().is_none());
}

#[tokio::test]
async fn a_stop_while_the_workspace_is_set_up_stops_the_converting_agent() {
    let (root, manager) = manager().await;
    let (workspace, mut process, _token) = converted(root.path(), &manager).await;
    let id = workspace.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute("UPDATE workspaces SET state='preparing' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    let targets = (manager.stop_targets(crate::protocol::QueuedLaunches::Keep))
        .await
        .unwrap();
    assert!(targets.iter().any(|target| target.id == workspace.id));

    let selector = SpaceSelector::id(&workspace.id);
    let keep = crate::protocol::QueuedLaunches::Keep;
    (manager.stop_workspace_manually(&selector, false, None, keep))
        .await
        .unwrap();

    process.wait().unwrap();
    let state = manager.workspace_by_id(&workspace.id).await.unwrap().state;
    assert_eq!(state, crate::state::WorkspaceState::Preparing);
}

#[tokio::test]
async fn the_relocation_restarts_no_agent_a_stop_handed_over() {
    let (root, manager) = manager().await;
    let (workspace, _process, _token) = converted(root.path(), &manager).await;
    let selector = SpaceSelector::id(&workspace.id);
    (manager.set_agent_state(&selector, AgentState::Idle, None, None))
        .await
        .unwrap();
    let keep = crate::protocol::QueuedLaunches::Keep;
    (manager.stop_workspace_manually(&selector, false, None, keep))
        .await
        .unwrap();

    manager.relocate_finished_turns().await.unwrap();

    assert!(RESUMED.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_workspace_whose_agent_exited_still_becomes_a_chat_with_its_session() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let identity = crate::process::identity::capture(process.id())
        .unwrap()
        .unwrap();
    process.kill();
    let execution = crate::client::execution_ids::ExecutionId::generate();
    // The agent exited on its own after the conversion was asked for,
    // keeping its session as a last session.
    let exited = LastSession {
        execution: execution.clone(),
        agent: "claude".into(),
        session: Some("s2".into()),
        moved: false,
        copied: None,
        converted: false,
        origin: None,
        stopped_by: None,
        stop: None,
        model: Default::default(),
        mode: ModeName::DEFAULT,
        narrowing: Default::default(),
    };
    exited.write(&manager.paths, &workspace.id).unwrap();
    let chat = crate::daemon::conversions::record::ConvertedChat {
        agent: "claude".into(),
        execution,
        session: Some("s1".into()),
        origin: None,
        process: identity,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
        stopped_by: None,
    };
    record_chat_conversion(&manager, &workspace, chat).await;

    manager.relocate_finished_turns().await.unwrap();

    let chat = manager.chat_by_id(&workspace.id).await.unwrap().unwrap();
    assert!(chat.converted);
    assert_eq!(
        chat.session.map(|session| session.to_string()),
        Some("s2".into())
    );
}

/// Record that `workspace` becomes `chat` once its agent's turn ends.
async fn record_chat_conversion(
    manager: &Manager,
    workspace: &Workspace,
    chat: crate::daemon::conversions::record::ConvertedChat,
) {
    let (id, pending) = (
        workspace.id.clone(),
        crate::daemon::conversions::Pending::Chat {
            chat: Box::new(chat),
            held: false,
        },
    );
    (manager.store)
        .write(move |tx| crate::daemon::conversions::record::record(tx, &id, &pending))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_stop_keeps_the_chat_a_workspace_becomes_down_for_its_stopper() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (lead, _lead) =
        crate::daemon::test_support::chat(&manager, "lead", "chat", Some(&repo), None).await;
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let identity = crate::process::identity::capture(process.id())
        .unwrap()
        .unwrap();
    process.kill();
    let chat = crate::daemon::conversions::record::ConvertedChat {
        agent: "claude".into(),
        execution: crate::client::execution_ids::ExecutionId::generate(),
        session: Some("s1".into()),
        origin: None,
        process: identity,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
        stopped_by: None,
    };
    record_chat_conversion(&manager, &workspace, chat).await;
    let selector = SpaceSelector::id(&workspace.id);
    let keep = crate::protocol::QueuedLaunches::Keep;
    (manager.stop_workspace_manually(&selector, false, Some(&lead.id), keep))
        .await
        .unwrap();

    manager.relocate_finished_turns().await.unwrap();

    let chat = manager.chat_by_id(&workspace.id).await.unwrap().unwrap();
    assert!(chat.restore.is_some());
    let resumed = crate::daemon::chats::restores::tests::RESUMED
        .lock()
        .unwrap()
        .clone();
    assert!(!resumed.contains(&workspace.id));
    let stopper = manager.chat_stopped_by(&workspace.id).await.unwrap();
    assert_eq!(stopper, Some(lead.id));
}

#[tokio::test]
async fn a_coordinators_stop_of_a_converting_agent_is_its_to_resume() {
    let (root, manager) = manager().await;
    let (workspace, mut process, _token) = converted(root.path(), &manager).await;
    let coordinator = crate::client::space_ids::SpaceId::generate();
    let selector = SpaceSelector::id(&workspace.id);
    let keep = crate::protocol::QueuedLaunches::Keep;

    (manager.stop_workspace_manually(&selector, false, Some(&coordinator), keep))
        .await
        .unwrap();

    process.wait().unwrap();
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    let stoppers: Vec<_> = sessions
        .iter()
        .map(|last| last.stopped_by.clone())
        .collect();
    assert_eq!(stoppers, [Some(coordinator)]);
}

#[tokio::test]
async fn the_processes_a_hand_over_signalled_outlive_it() {
    let (root, manager) = manager().await;
    let (workspace, _process, _token) = converted(root.path(), &manager).await;
    let identity = |pid: u32| crate::process::identity::Identity {
        pid,
        birth: format!("birth-{pid}"),
        unique: None,
    };
    let id = workspace.id.clone();
    let (first, second) = (
        vec![identity(1), identity(2)],
        vec![identity(2), identity(3)],
    );

    let stopped = (manager.store)
        .write(move |tx| {
            crate::daemon::conversions::record::record_stopped(tx, &id, &first)?;
            crate::daemon::conversions::record::record_stopped(tx, &id, &second)?;
            crate::daemon::conversions::record::stopped_processes(tx, &id)
        })
        .await
        .unwrap();

    let pids: Vec<_> = stopped.iter().map(|process| process.pid).collect();
    assert_eq!(pids, [1, 2, 3]);
    // The conversion itself reads as it did.
    let id = workspace.id.clone();
    let pending = (manager.store)
        .run(move |db| crate::daemon::conversions::pending(db, &id))
        .await
        .unwrap();
    assert!(pending.is_some());
}

#[tokio::test]
async fn a_stop_while_the_relocation_waits_keeps_the_chat_down() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let identity = crate::process::identity::capture(process.id())
        .unwrap()
        .unwrap();
    process.kill();
    let chat = crate::daemon::conversions::record::ConvertedChat {
        agent: "claude".into(),
        execution: crate::client::execution_ids::ExecutionId::generate(),
        session: Some("s1".into()),
        origin: None,
        process: identity,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
        stopped_by: None,
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
            manager.conversion_handovers.hold(stopping).await;
        }
    });
    holding.await.unwrap();
    let relocating = tokio::spawn({
        let manager = manager.clone();
        async move { manager.relocate_finished_turns().await }
    });
    // The relocation lists the conversion, then waits for the hand-overs,
    // which a stop holds while it holds the conversion.
    LISTED.notified().await;
    let id = workspace.id.clone();
    (manager.store)
        .run(move |db| crate::daemon::conversions::record::hold_chat(db, &id, None))
        .await
        .unwrap();
    release.send(()).unwrap();
    stop.await.unwrap();

    relocating.await.unwrap().unwrap();

    let chat = manager.chat_by_id(&workspace.id).await.unwrap().unwrap();
    assert!(chat.restore.is_some());
    let resumed = crate::daemon::chats::restores::tests::RESUMED
        .lock()
        .unwrap()
        .clone();
    assert!(!resumed.contains(&workspace.id));
}

#[tokio::test]
async fn the_hand_over_continues_the_session_reported_after_the_conversion() {
    let (root, manager) = manager().await;
    let (workspace, mut process, _token) = converted(root.path(), &manager).await;
    let selector = SpaceSelector::id(&workspace.id);
    let idle = AgentState::Idle;

    (manager.set_agent_state(&selector, idle, None, Some("s3".into())))
        .await
        .unwrap();
    manager.relocate_finished_turns().await.unwrap();

    process.wait().unwrap();
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    let reported: Vec<_> = (sessions.iter())
        .map(|last| last.session.as_ref().map(ToString::to_string))
        .collect();
    assert_eq!(reported, [Some("s3".to_owned())]);
}

#[tokio::test]
async fn a_relocation_hands_over_the_converting_agents_its_removal_reaches() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "base".into(), None, None, None)
        .await
        .unwrap();
    // A chat in its worktree, below which a chat became a workspace whose
    // agent has yet to restart there.
    let inside_process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let inside = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "inside".into(),
            "claude".into(),
            inside_process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let (below, mut below_process) = crate::daemon::test_support::chat(
        &manager,
        "below",
        "worker",
        Some(&repo),
        Some(&inside.id),
    )
    .await;
    let converting = Converting {
        chat: below,
        mode: ModeName::DEFAULT,
    };
    let converted = manager
        .create_converted_workspace(converting, "part".into(), None, Default::default())
        .await
        .unwrap();
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let identity = crate::process::identity::capture(process.id())
        .unwrap()
        .unwrap();
    process.kill();
    let chat = crate::daemon::conversions::record::ConvertedChat {
        agent: "claude".into(),
        execution: crate::client::execution_ids::ExecutionId::generate(),
        session: Some("s1".into()),
        origin: None,
        process: identity,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
        stopped_by: None,
    };
    record_chat_conversion(&manager, &workspace, chat).await;

    let relocated = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        manager.relocate_finished_turns(),
    )
    .await
    .expect("the relocation's removal takes the hand-overs it holds");

    relocated.unwrap();
    below_process.wait().unwrap();
    assert!(manager.chat_by_id(&workspace.id).await.unwrap().is_some());
    assert!(manager.workspace_by_id(&converted.id).await.is_err());
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
    let operation = manager.start_operation(spec, None, None).await.unwrap();
    let (id, recorded) = (workspace.id.clone(), operation.clone());
    (manager.store)
        .run(move |db| crate::daemon::conversions::record::record_preparation(db, &id, &recorded))
        .await
        .unwrap();
    let worker = crate::daemon::operations::fake::take(&operation);
    (operation, worker)
}

#[tokio::test]
async fn the_agent_waits_for_its_workspaces_post_setup_hook() {
    let (root, manager) = manager().await;
    let (workspace, _process, token) = converted(root.path(), &manager).await;
    let (_operation, _worker) = record_preparation(&manager, &workspace).await;
    let selector = SpaceSelector::id(&workspace.id);
    (manager.set_agent_state(&selector, AgentState::Idle, None, None))
        .await
        .unwrap();

    manager.relocate_finished_turns().await.unwrap();

    assert!(RESUMED.lock().unwrap().is_empty());
    assert!(manager.caller(&token).await.unwrap().is_some());
    assert!(manager.converting(&workspace.id).await.unwrap());
}

#[tokio::test]
async fn a_failed_post_setup_hook_stops_the_agent_for_shoal_resume() {
    use crate::protocol::operations::{Ended, Failure, Report};
    let (root, manager) = manager().await;
    let (workspace, mut process, token) = converted(root.path(), &manager).await;
    let (operation, worker) = record_preparation(&manager, &workspace).await;
    let failed = Ended::Failed(Failure::HookFailed {
        message: "hook exited 1".into(),
    });
    (manager.report_operation(&operation, Report::Ended(failed), None))
        .await
        .unwrap();
    worker.exit(1);
    let entry = manager.operation(&operation, None).await.unwrap();
    let mut changed = entry.watch();
    while entry.events_from(0).1.is_none() {
        changed.changed().await.unwrap();
    }
    let selector = SpaceSelector::id(&workspace.id);
    (manager.set_agent_state(&selector, AgentState::Idle, None, None))
        .await
        .unwrap();

    manager.relocate_finished_turns().await.unwrap();

    // Stopped with its session kept for shoal resume, not restarted.
    process.wait().unwrap();
    assert!(RESUMED.lock().unwrap().is_empty());
    assert!(manager.caller(&token).await.unwrap().is_none());
    assert!(!manager.converting(&workspace.id).await.unwrap());
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
}
