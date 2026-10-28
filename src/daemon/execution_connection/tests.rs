use super::*;
use crate::test_support::{manager, repository};
use tokio::io::BufReader;

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(60), future)
        .await
        .expect("execution connection stalled")
}

async fn read<T: serde::de::DeserializeOwned>(stream: &mut BufReader<UnixStream>) -> T {
    bounded(protocol::read_buffered(stream)).await.unwrap()
}

#[tokio::test]
async fn scoped_landing_answers_with_its_approval_request() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "lander".into(), None, None, None)
        .await
        .unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let executing = tokio::spawn(execute(
        server,
        3,
        manager.clone(),
        ExecutionContext {
            workspace: workspace.id.clone(),
            wrapper: crate::process::identity::capture(std::process::id())
                .unwrap()
                .unwrap(),
            kind: ExecutionKind::Land,
            agent: None,
            recover: false,
            reattach: false,
            queue_entry: None,
            demo: None,
            caller: Some(Caller {
                workspace_id: workspace.id.clone(),
                execution: None,
            }),
        },
        manager.gates.owned_operation().await,
    ));
    let response: Response = read(&mut BufReader::new(client)).await;
    bounded(executing).await.unwrap().unwrap();
    let Body::ApprovalRequest(request) = response.body else {
        panic!("landing was not held for approval: {:?}", response.body);
    };
    assert_eq!(request.target, crate::daemon::approvals::Target::Land);
    let notifications = manager.notifications(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|n| n.message.contains(&format!("approval {}:", request.id))),
        "{notifications:?}"
    );
}

#[tokio::test]
async fn shutdown_detaches_reattached_wrappers_and_stops_older_ones() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
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
        .begin_execution(
            &workspace.id,
            Some(wrapper.clone()),
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let detaching = started.plan.id.clone();
    manager
        .record_execution_child(detaching.clone(), Some(wrapper.clone()), wrapper.pid)
        .await
        .unwrap();
    manager.detach_execution(&detaching).await;
    let (client, server) = UnixStream::pair().unwrap();
    let reattached = tokio::spawn(reattach(
        server,
        7,
        manager.clone(),
        Reattach {
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
        manager.clone(),
        ExecutionContext {
            workspace: workspace.id.clone(),
            wrapper: wrapper.clone(),
            kind: ExecutionKind::Command,
            agent: None,
            recover: false,
            reattach: false,
            queue_entry: None,
            demo: None,
            caller: None,
        },
        manager.gates.owned_operation().await,
    ));
    let mut legacy = BufReader::new(client);
    let response: Response = read(&mut legacy).await;
    assert!(manager.executions.has_unattachable());
    let Body::Execution(plan) = response.body else {
        panic!("execution was not registered");
    };
    protocol::write(
        legacy.get_mut(),
        &ExecutionEvent::Started {
            child: None,
            group_id: wrapper.pid,
        },
    )
    .await
    .unwrap();
    assert!(matches!(read(&mut legacy).await, Control::Started));
    let shutdown = tokio::spawn({
        let manager = manager.clone();
        async move { manager.stop_for_shutdown().await }
    });
    assert!(matches!(read(&mut capable).await, Control::Detach));
    bounded(reattached).await.unwrap().unwrap();
    assert!(!manager.executions.is_connected(&detaching).await);
    assert!(matches!(
        read(&mut legacy).await,
        Control::Pause { reason: Some(reason), removed: false } if reason == "the Shoal daemon shut down"
    ));
    protocol::write(legacy.get_mut(), &ExecutionEvent::Finished { exit_code: 0 })
        .await
        .unwrap();
    assert!(matches!(
        read(&mut legacy).await,
        Control::Finished { complete: true }
    ));
    bounded(executing).await.unwrap().unwrap();
    bounded(shutdown).await.unwrap();
    assert!(!manager.executions.has_unattachable());
    // The detached execution awaits reattachment; the stopped one is gone.
    let executions = manager
        .inspect_workspace(&workspace.id)
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
        .register_repository(path.to_str().unwrap().into(), None, None)
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
        manager.clone(),
        ExecutionContext {
            workspace: workspace.id.clone(),
            wrapper: wrapper.clone(),
            kind: ExecutionKind::Command,
            agent: None,
            recover: false,
            reattach: true,
            caller: None,
            queue_entry: None,
            demo: None,
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
        &ExecutionEvent::Finished { exit_code: 0 },
    )
    .await
    .unwrap();
    bounded(executing).await.unwrap().unwrap();
    drop(update);
    let executions = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions;
    assert_eq!(executions.len(), 1);
    assert!(!manager.executions.is_connected(&executions[0].id).await);
}
