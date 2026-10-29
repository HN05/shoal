use std::time::Duration;

use tokio::{io::BufReader, net::UnixStream};

use super::{super::ExecutionKind, *};
use crate::{
    daemon::execution_connection::{ExecutionContext, execute},
    protocol::{self, Body, Control, ExecutionEvent, Response},
    test_support::{manager, repository},
};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("system shutdown stop stalled")
}

async fn read<T: serde::de::DeserializeOwned>(stream: &mut BufReader<UnixStream>) -> T {
    bounded(protocol::read_buffered(stream)).await.unwrap()
}

#[tokio::test]
async fn a_shutdown_stops_running_work_and_restores_it_only_once_cancelled() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
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
            reattach: false,
            queue_entry: None,
            demo: None,
            origin: None,
            caller: None,
        },
        manager.gates.owned_operation().await,
    ));
    let mut connection = BufReader::new(client);
    let response: Response = read(&mut connection).await;
    let Body::Execution(plan) = response.body else {
        panic!("execution was not registered");
    };
    let started = ExecutionEvent::Started {
        child: None,
        group_id: wrapper.pid,
        restored: false,
    };
    protocol::write(connection.get_mut(), &started)
        .await
        .unwrap();
    assert!(matches!(read(&mut connection).await, Control::Started));

    // An agent stopped earlier whose restore is already in the queue.
    recovery::save_record(
        &manager.paths,
        &workspace.id,
        "earlier",
        "claude",
        None,
        None,
    )
    .unwrap();
    manager
        .queue_stopped_restore(&workspace.id, "earlier", Default::default())
        .await
        .unwrap();

    let stopping = tokio::spawn({
        let manager = manager.clone();
        async move { manager.stop_for_system_shutdown().await }
    });
    assert!(matches!(
        read(&mut connection).await,
        Control::Pause { reason: Some(reason), removed: false, shutdown: true }
            if reason == "the system is shutting down"
    ));
    assert!(manager.agent_queue.suspended());
    assert!(
        manager.gates.try_quiesce().is_none(),
        "an update must wait for the stop"
    );
    // The wrapper saves its agent and requests the restore.
    recovery::save_record(
        &manager.paths,
        &workspace.id,
        &plan.id,
        "claude",
        None,
        None,
    )
    .unwrap();
    let request = manager
        .paths
        .workspace_state(&workspace.id)
        .join(format!("{}.restore-request.json", plan.id));
    std::fs::write(&request, "{}").unwrap();
    let finished = ExecutionEvent::Finished { exit_code: 143 };
    protocol::write(connection.get_mut(), &finished)
        .await
        .unwrap();
    assert!(matches!(
        read(&mut connection).await,
        Control::Finished { complete: true }
    ));
    bounded(executing).await.unwrap().unwrap();
    bounded(stopping).await.unwrap();

    let notifications = manager.notifications(true, 10).await.unwrap();
    let stopped: Vec<_> = notifications
        .iter()
        .filter(|n| n.kind == NotificationKind::SystemShutdown)
        .map(|n| n.message.as_str())
        .collect();
    assert_eq!(
        stopped,
        [
            "Stopped work in 1 workspace before the system shut down; the agent queue restores its agents"
        ]
    );
    // The stopped agent stays stopped work with its request, not a last session.
    let saved = Saved::load(&manager.paths, &workspace.id).unwrap();
    assert_eq!(saved.agents.len(), 2);
    assert!(saved.sessions.is_empty());
    assert!(request.exists());

    // Both restores queue, but the queue starts them only once the shutdown
    // is cancelled, the earlier first.
    let restores = async || {
        manager.dispatch_queue().await.unwrap();
        let entries = manager.queue_entries().await.unwrap();
        let state = |execution: &str| {
            let entry = entries
                .iter()
                .find(|entry| entry.execution.as_deref() == Some(execution));
            entry.expect("the restore was not queued").state
        };
        (state("earlier"), state(&plan.id))
    };
    use crate::state::QueueState::{Queued, Starting};
    assert_eq!(restores().await, (Queued, Queued));
    manager.system_shutdown_cancelled().await;
    assert!(!manager.agent_queue.suspended());
    assert_eq!(restores().await.0, Starting);
}

#[tokio::test]
async fn suspending_waits_for_an_admission_pass_in_progress() {
    use futures_util::FutureExt;
    let queue = super::super::agent_queue::AgentQueue::default();
    let admitting = queue.lock_admission().await;
    let mut suspending = Box::pin(queue.suspend(true));
    assert!(suspending.as_mut().now_or_never().is_none());
    assert!(!queue.suspended());
    drop(admitting);
    suspending.await;
    assert!(queue.suspended());
}

#[test]
fn only_requested_or_queued_restores_continue_without_the_user() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    let none = HashSet::new();
    assert!(!waits_for_resume(&paths, "workspace", &none).unwrap());
    recovery::save_record(&paths, "workspace", "requested", "claude", None, None).unwrap();
    let request = paths
        .workspace_state("workspace")
        .join("requested.restore-request.json");
    std::fs::write(&request, "{}").unwrap();
    assert!(!waits_for_resume(&paths, "workspace", &none).unwrap());
    recovery::save_record(&paths, "workspace", "unattended", "claude", None, None).unwrap();
    assert!(waits_for_resume(&paths, "workspace", &none).unwrap());
    let queued = HashSet::from(["unattended".to_owned()]);
    assert!(!waits_for_resume(&paths, "workspace", &queued).unwrap());
}
