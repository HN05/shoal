use crate::daemon::execution_connection::*;
use crate::daemon::scope::Access;
use crate::daemon::test_support::{manager, repository};
use crate::daemon::{executions::ExecutionKind, manager::Manager, scope::Caller};
use crate::protocol::{
    self, Body, Control, ErrorCode, ExecutionEvent, Reattach, Response, ScopeMemory,
};
use anyhow::Result;
use shoal_protocol::executions::TrackedAgent;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::stop_causes::{EndSignal, SignalTarget, StopCause};
use shoal_protocol::threads::ThreadOpening;
use std::sync::Arc;
use tokio::io::BufReader;
use tokio::net::UnixStream;

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(60), future)
        .await
        .expect("execution connection stalled")
}

async fn read<T: serde::de::DeserializeOwned>(stream: &mut BufReader<UnixStream>) -> T {
    bounded(protocol::read_buffered(stream)).await.unwrap()
}

#[tokio::test]
async fn scoped_merge_answers_with_its_approval_request() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "merger".into(), None, None, None)
        .await
        .unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let executing = tokio::spawn(execute(
        server,
        3,
        manager.agents.clone(),
        manager.clone(),
        ExecutionContext {
            space: SpaceSelector::id(&workspace.id),
            wrapper: crate::process::identity::capture(std::process::id())
                .unwrap()
                .unwrap(),
            kind: ExecutionKind::Merge,
            agent: None,
            recover: false,
            reattach: false,
            queue_entry: None,
            demo: None,
            origin: None,
            merge: None,
            caller: Some(Caller {
                space_id: workspace.id.clone(),
                execution: None,
                granted: None,
                access: Access::Full,
                vouched: None,
            }),
        },
        manager.gates.owned_operation().await,
    ));
    let response: Response = read(&mut BufReader::new(client)).await;
    bounded(executing).await.unwrap().unwrap();
    let Body::ApprovalRequest(request) = response.body else {
        panic!("merge was not held for approval: {:?}", response.body);
    };
    assert_eq!(request.target, crate::daemon::approvals::Target::Merge);
    let notifications = manager.notifications.list(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|n| n.kind == crate::daemon::notifications::AlertKind::ApprovalRequested),
        "{notifications:?}"
    );
}

#[tokio::test]
async fn shutdown_detaches_reattached_wrappers_and_stops_older_ones() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "detach".into(), None, None, None)
        .await
        .unwrap();
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    // A capable wrapper from before the restart.
    let started = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            Some(wrapper.clone()),
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let detaching = started.plan.id.clone();
    manager
        .agents
        .record_execution_child(detaching.clone(), Some(wrapper.clone()), wrapper.pid)
        .await
        .unwrap();
    manager.agents.detach_execution(&detaching).await;
    let (client, server) = UnixStream::pair().unwrap();
    let reattached = tokio::spawn(reattach(
        server,
        7,
        crate::protocol::VERSION,
        manager.agents.clone(),
        manager.clone(),
        Reattach {
            narrowing: Default::default(),
            mode: None,
            model: Default::default(),
            execution: detaching.clone(),
            wrapper: wrapper.clone(),
            child: Some(wrapper.clone()),
            group_id: wrapper.pid,
            scope_token: started.plan.scope_token.clone(),
            agent: None,
            recover: false,
            running_ms: 0,
            awaiting_restore: None,
            between_runs: false,
            reports_status: false,
        },
        manager.gates.owned_operation().await,
    ));
    let mut capable = BufReader::new(client);
    let response: Response = read(&mut capable).await;
    assert!(matches!(response.body, Body::Ok) && response.id == 7);
    // A wrapper that cannot reattach, registered over the same protocol.
    let (client, server) = UnixStream::pair().unwrap();
    let executing = tokio::spawn(execute(
        server,
        8,
        manager.agents.clone(),
        manager.clone(),
        ExecutionContext {
            space: SpaceSelector::id(&workspace.id),
            wrapper: wrapper.clone(),
            kind: ExecutionKind::Command,
            agent: None,
            recover: false,
            reattach: false,
            queue_entry: None,
            demo: None,
            origin: None,
            merge: None,
            caller: None,
        },
        manager.gates.owned_operation().await,
    ));
    let mut legacy = BufReader::new(client);
    let response: Response = read(&mut legacy).await;
    assert!(manager.agents.executions().has_unattachable());
    let Body::Execution(plan) = response.body else {
        panic!("execution was not registered");
    };
    protocol::write(
        legacy.get_mut(),
        &ExecutionEvent::Started {
            child: None,
            group_id: wrapper.pid,
            restored: false,
            restored_from: Vec::new(),
            reports_status: false,
        },
    )
    .await
    .unwrap();
    assert!(matches!(read(&mut legacy).await, Control::Started));
    let shutdown = tokio::spawn({
        let manager = manager.clone();
        async move { manager.agents.stop_for_shutdown().await }
    });
    assert!(matches!(read(&mut capable).await, Control::Detach));
    bounded(reattached).await.unwrap().unwrap();
    assert!(!manager.agents.executions().is_connected(&detaching).await);
    assert!(matches!(
        read(&mut legacy).await,
        Control::Pause {
            reason: Some(_),
            removed: false,
            shutdown: false
        }
    ));
    protocol::write(
        legacy.get_mut(),
        &ExecutionEvent::Finished {
            exit_code: 0,
            memory: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read(&mut legacy).await,
        Control::Finished { complete: true }
    ));
    bounded(executing).await.unwrap().unwrap();
    bounded(shutdown).await.unwrap();
    assert!(!manager.agents.executions().has_unattachable());
    // The detached execution awaits reattachment; the stopped one is gone.
    let executions = manager
        .spaces
        .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .executions;
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].id, detaching);
    assert_ne!(executions[0].id, plan.id);
}

#[tokio::test]
async fn running_executions_leave_daemon_operations_idle() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "idle".into(), None, None, None)
        .await
        .unwrap();
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let executing = tokio::spawn(execute(
        server,
        1,
        manager.agents.clone(),
        manager.clone(),
        ExecutionContext {
            space: SpaceSelector::id(&workspace.id),
            wrapper: wrapper.clone(),
            kind: ExecutionKind::Command,
            agent: None,
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
    let response: Response = read(&mut wrapper_side).await;
    assert!(matches!(response.body, Body::Execution(_)));
    // Until its child is recorded, the wrapper cannot reattach.
    assert!(manager.gates.try_quiesce().is_none());
    protocol::write(
        wrapper_side.get_mut(),
        &ExecutionEvent::Started {
            child: None,
            group_id: wrapper.pid,
            restored: false,
            restored_from: Vec::new(),
            reports_status: false,
        },
    )
    .await
    .unwrap();
    assert!(matches!(read(&mut wrapper_side).await, Control::Started));
    // An update can take every operation while the command still runs.
    let update = bounded(manager.gates.quiesce()).await;
    // A command exiting meanwhile reports to the replacement daemon.
    protocol::write(
        wrapper_side.get_mut(),
        &ExecutionEvent::Finished {
            exit_code: 0,
            memory: None,
        },
    )
    .await
    .unwrap();
    bounded(executing).await.unwrap().unwrap();
    drop(update);
    let executions = manager
        .spaces
        .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .executions;
    assert_eq!(executions.len(), 1);
    assert!(
        !manager
            .agents
            .executions()
            .is_connected(&executions[0].id)
            .await
    );
}

#[tokio::test]
async fn a_wrapper_registers_again_from_where_it_runs() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "upgraded".into(), None, None, None)
        .await
        .unwrap();
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    // A wrapper of an earlier release reported no place.
    let (client, server) = UnixStream::pair().unwrap();
    let executing = tokio::spawn(execute(
        server,
        1,
        manager.agents.clone(),
        manager.clone(),
        ExecutionContext {
            space: SpaceSelector::id(&workspace.id),
            wrapper,
            kind: ExecutionKind::Command,
            agent: None,
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
    let response: Response = read(&mut wrapper_side).await;
    let Body::Execution(registered) = response.body else {
        panic!("execution was not registered: {:?}", response.body);
    };
    let origin = crate::sessions::Origin::created(crate::sessions::HostName::Zmx, "agent".into());
    protocol::write(
        wrapper_side.get_mut(),
        &ExecutionEvent::Register {
            origin: Some(Box::new(origin.clone())),
        },
    )
    .await
    .unwrap();
    let Control::Registered { plan } = read(&mut wrapper_side).await else {
        panic!("the wrapper did not register again");
    };
    assert_eq!(plan.id, registered.id);
    assert_eq!(plan.scope_token, registered.scope_token);
    assert_eq!(plan.environment, registered.environment);
    let executions = manager
        .spaces
        .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .executions;
    assert_eq!(executions[0].origin, Some(origin));
    protocol::write(
        wrapper_side.get_mut(),
        &ExecutionEvent::Finished {
            exit_code: 0,
            memory: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read(&mut wrapper_side).await,
        Control::Finished { .. }
    ));
    bounded(executing).await.unwrap().unwrap();
}

/// The refusal of an agent's start in `workspace`, which leaves it no new
/// execution.
async fn refused_agent(manager: &Arc<Manager>, workspace: &crate::model::Workspace) -> String {
    let selector = SpaceSelector::id(&workspace.id);
    let ids = |executions: Vec<crate::model::Execution>| -> Vec<_> {
        executions
            .into_iter()
            .map(|execution| execution.id)
            .collect()
    };
    let recorded = ids(manager
        .spaces
        .inspect_workspace(&**manager, &selector)
        .await
        .unwrap()
        .executions);
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let executing = tokio::spawn(execute(
        server,
        1,
        manager.agents.clone(),
        manager.clone(),
        ExecutionContext {
            space: SpaceSelector::id(&workspace.id),
            wrapper,
            kind: ExecutionKind::Command,
            agent: Some(shoal_protocol::executions::TrackedAgent::named("claude")),
            recover: true,
            reattach: false,
            caller: None,
            queue_entry: None,
            demo: None,
            origin: None,
            merge: None,
        },
        manager.gates.owned_operation().await,
    ));
    let response: Response = read(&mut BufReader::new(client)).await;
    bounded(executing).await.unwrap().unwrap();
    let Body::Error { message, .. } = response.body else {
        panic!("the agent was admitted: {:?}", response.body);
    };
    let executions = ids(manager
        .spaces
        .inspect_workspace(&**manager, &selector)
        .await
        .unwrap()
        .executions);
    assert_eq!(executions, recorded);
    message
}

#[tokio::test]
async fn no_agent_starts_in_a_workspace_that_becomes_a_chat() {
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
    // Its agent stopped, and the relocation has yet to make it the chat.
    let chat = crate::daemon::conversions::pending::ConvertedChat {
        agent: "claude".into(),
        execution: shoal_ids::execution_ids::ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    let pending =
        crate::daemon::conversions::Pending::Chat(crate::daemon::conversions::pending::ToChat {
            chat: Box::new(chat),
            held: true,
            carrying: Default::default(),
        });
    let id = workspace.id.clone();
    (manager.store)
        .write(move |tx| crate::daemon::conversions::pending::record(tx, &id, &pending))
        .await
        .unwrap();

    let refused = refused_agent(&manager, &workspace).await;

    assert!(refused.contains("becomes a chat"), "{refused}");
}

#[tokio::test]
async fn no_agent_starts_in_a_workspace_a_chats_agent_moves_to() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let talk =
        crate::daemon::test_support::chat(&manager, "talk", "worker", Some(&repo), None).await;
    let converting = crate::daemon::conversions::Converting {
        chat: talk,
        mode: crate::config::modes::ModeName::DEFAULT,
        carrying: Default::default(),
    };
    // The chat's agent works on where it ran, its turn not yet ended.
    let workspace = manager
        .spaces
        .create_converted_workspace(
            &*manager,
            converting,
            "fix-login".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();

    let refused = refused_agent(&manager, &workspace).await;

    assert!(refused.contains("moves there"), "{refused}");
}

/// The daemon's first answer to a command in `workspace` whose wrapper
/// cannot reattach, as `shoal setup` and merges register, and the task
/// serving it.
async fn register_unattachable(
    manager: &Arc<Manager>,
    workspace: &crate::model::Workspace,
) -> (
    Body,
    BufReader<UnixStream>,
    tokio::task::JoinHandle<Result<()>>,
) {
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let executing = tokio::spawn(execute(
        server,
        9,
        manager.agents.clone(),
        manager.clone(),
        ExecutionContext {
            space: SpaceSelector::id(&workspace.id),
            wrapper,
            kind: ExecutionKind::Command,
            agent: None,
            recover: false,
            reattach: false,
            queue_entry: None,
            demo: None,
            origin: None,
            merge: None,
            caller: None,
        },
        manager.gates.owned_operation().await,
    ));
    let mut client = BufReader::new(client);
    let response: Response = read(&mut client).await;
    (response.body, client, executing)
}

#[tokio::test]
async fn an_exit_that_waits_refuses_commands_that_would_stop_with_it() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "held".into(), None, None, None)
        .await
        .unwrap();
    let _held = manager.gates.hold_back(crate::protocol::Exit::Restart);
    let (body, _client, executing) = register_unattachable(&manager, &workspace).await;
    let Body::Error { code, .. } = body else {
        panic!("the command was registered");
    };
    assert_eq!(code, ErrorCode::Exiting);
    assert!(!manager.agents.executions().has_unattachable());
    let _ = bounded(executing).await;

    // The setup of an operation the exit waits for still registers.
    let spec = crate::protocol::operations::OperationSpec::Prepare {
        workspace: SpaceSelector::id(&workspace.id),
        steps: Default::default(),
    };
    let operation = manager
        .spaces
        .start_operation(spec, None, None)
        .await
        .unwrap();
    let (body, _client, executing) = register_unattachable(&manager, &workspace).await;
    assert!(matches!(body, Body::Execution(_)), "{body:?}");
    assert!(manager.agents.executions().has_unattachable());
    executing.abort();
    crate::daemon::operations::fake::take(&operation).exit(0);
}

/// The next status report `wrapper` receives, past its start's
/// acknowledgement.
async fn next_status(
    wrapper: &mut BufReader<UnixStream>,
) -> shoal_protocol::program_status::ProgramStatus {
    loop {
        match read(wrapper).await {
            Control::ProgramStatus { status } => return status,
            Control::Started => {}
            control => panic!("expected a status report, got {control:?}"),
        }
    }
}

#[tokio::test]
async fn a_wrapper_whose_terminal_reads_status_reports_hears_the_spaces_status() {
    use shoal_protocol::program_status::{ProgramState, Waits};
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "status".into(), None, None, None)
        .await
        .unwrap();
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let executing = tokio::spawn(execute(
        server,
        1,
        manager.agents.clone(),
        manager.clone(),
        ExecutionContext {
            space: SpaceSelector::id(&workspace.id),
            wrapper: wrapper.clone(),
            kind: ExecutionKind::Command,
            agent: Some(TrackedAgent::named("codex")),
            recover: false,
            reattach: false,
            queue_entry: None,
            demo: None,
            origin: None,
            merge: None,
            caller: None,
        },
        manager.gates.owned_operation().await,
    ));
    let mut wrapper_side = BufReader::new(client);
    let response: Response = read(&mut wrapper_side).await;
    assert!(matches!(response.body, Body::Execution(_)));
    protocol::write(
        wrapper_side.get_mut(),
        &ExecutionEvent::Started {
            child: None,
            group_id: wrapper.pid,
            restored: false,
            restored_from: Vec::new(),
            reports_status: true,
        },
    )
    .await
    .unwrap();
    let status = next_status(&mut wrapper_side).await;
    assert_eq!(
        (status.state, status.title.as_str()),
        (ProgramState::Clear, "status")
    );
    // The space asks the user, which blocks it on them.
    let asking = crate::daemon::test_support::caller_in(&SpaceSelector::id(&workspace.id));
    let thread = manager
        .coordination
        .open_thread(
            &*manager,
            Some(&asking),
            ThreadOpening::named("Database"),
            Some(shoal_protocol::threads::ThreadKind::Question),
            None,
        )
        .await
        .unwrap();
    let question = shoal_protocol::threads::NewPost {
        text: "Which database?\nSQLite needs no server.".into(),
        ..Default::default()
    };
    manager
        .coordination
        .post_to_thread(
            &*manager,
            Some(&asking),
            shoal_protocol::threads::ThreadNumber::Full(thread.number),
            question,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .await
        .unwrap();
    let status = next_status(&mut wrapper_side).await;
    assert_eq!(
        status.state,
        ProgramState::Blocked {
            kind: Waits::Question
        }
    );
    assert_eq!(
        status.message,
        Some(format!("{}: Which database?", thread.number))
    );
    protocol::write(
        wrapper_side.get_mut(),
        &ExecutionEvent::Finished {
            exit_code: 0,
            memory: None,
        },
    )
    .await
    .unwrap();
    loop {
        match read(&mut wrapper_side).await {
            Control::Finished { .. } => break,
            Control::ProgramStatus { .. } => {}
            control => panic!("expected the execution to finish, got {control:?}"),
        }
    }
    bounded(executing).await.unwrap().unwrap();
}

mod registration;

/// The kills an agent's log records on its stop or exit and its last
/// alert, once a run that a pause with 2 kills stopped finished without a
/// memory report.
async fn kills_after_a_pause() -> (Vec<(String, Option<i64>, Option<String>)>, String) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "killed".into(), None, None, None)
        .await
        .unwrap();
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let executing = tokio::spawn(execute(
        server,
        1,
        manager.agents.clone(),
        manager.clone(),
        ExecutionContext {
            space: SpaceSelector::id(&workspace.id),
            wrapper: wrapper.clone(),
            kind: ExecutionKind::Command,
            agent: Some(TrackedAgent::named("claude")),
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
    let response: Response = read(&mut wrapper_side).await;
    assert!(matches!(response.body, Body::Execution(_)));
    let started = ExecutionEvent::Started {
        child: None,
        group_id: wrapper.pid,
        restored: false,
        restored_from: Vec::new(),
        reports_status: false,
    };
    let paused = ExecutionEvent::Paused {
        memory: Some(ScopeMemory {
            oom_kills: 2,
            peak: 1 << 30,
        }),
        cause: Some(StopCause::Signal {
            signal: EndSignal::Terminate,
            reached: SignalTarget::Wrapper,
        }),
    };
    for event in [started, paused] {
        protocol::write(wrapper_side.get_mut(), &event)
            .await
            .unwrap();
    }
    assert!(matches!(read(&mut wrapper_side).await, Control::Started));
    // The wrapper then finishes, its paused run already reported.
    let finished = ExecutionEvent::Finished {
        exit_code: 143,
        memory: None,
    };
    protocol::write(wrapper_side.get_mut(), &finished)
        .await
        .unwrap();
    assert!(matches!(
        read(&mut wrapper_side).await,
        Control::Finished { .. }
    ));
    bounded(executing).await.unwrap().unwrap();
    let id = workspace.id.clone();
    let kills = (manager.store)
        .run(move |db| {
            let mut query = db.prepare(
                "SELECT json_extract(record,'$.kind'), json_extract(record,'$.agent.oom_kills'),
                     json_extract(record,'$.agent.cause.signal')
                 FROM space_events WHERE space_id=?1
                 AND json_extract(record,'$.kind') IN ('agent_stopped','agent_exited')",
            )?;
            Ok(query
                .query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    let alerts = manager.notifications.list(false, 1).await.unwrap();
    (kills, alerts[0].message.clone())
}

#[tokio::test]
async fn an_agents_pause_records_the_kills_of_the_run_it_stopped_and_its_cause() {
    let (kills, alert) = kills_after_a_pause().await;
    assert_eq!(
        kills,
        [(
            "agent_stopped".to_owned(),
            Some(2),
            Some("SIGTERM".to_owned())
        )]
    );
    assert!(alert.contains("killed 2"), "{alert}");
}
