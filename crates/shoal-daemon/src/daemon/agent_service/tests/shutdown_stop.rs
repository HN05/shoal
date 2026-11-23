use crate::execution::recovery::{self, Saved, SavedExt as _};
use crate::paths::StateFiles;
use shoal_daemon_base::alerts::AlertKind;
use shoal_ids::execution_ids::ExecutionId;
use std::time::Duration;

use tokio::{io::BufReader, net::UnixStream};

use crate::daemon::executions::ExecutionKind;
use crate::{
    daemon::{
        execution_connection::{ExecutionContext, execute},
        test_support::{manager, repository},
    },
    protocol::{self, Body, Control, ExecutionEvent, Response},
};
use shoal_protocol::space_names::SpaceSelector;

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
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
    let mut connection = BufReader::new(client);
    let response: Response = read(&mut connection).await;
    let Body::Execution(plan) = response.body else {
        panic!("execution was not registered");
    };
    let started = ExecutionEvent::Started {
        child: None,
        group_id: wrapper.pid,
        restored: false,
        restored_from: Vec::new(),
        reports_status: false,
    };
    protocol::write(connection.get_mut(), &started)
        .await
        .unwrap();
    assert!(matches!(read(&mut connection).await, Control::Started));

    // An agent stopped earlier whose restore is already in the queue.
    recovery::save_record(
        &manager.paths,
        &workspace.id,
        &ExecutionId::from("earlier"),
        &shoal_protocol::executions::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    manager
        .agents
        .queue_stopped_restore(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            &ExecutionId::from("earlier"),
            Default::default(),
        )
        .await
        .unwrap();

    let stopping = tokio::spawn({
        let manager = manager.clone();
        async move {
            let window = Duration::from_secs(60);
            (manager.agents)
                .stop_for_system_shutdown(&*manager, window)
                .await
        }
    });
    assert!(matches!(
        read(&mut connection).await,
        Control::Pause {
            removed: false,
            shutdown: true,
            ..
        }
    ));
    assert!(manager.agents.agent_queue().suspended());
    assert!(
        manager.gates.try_quiesce().is_none(),
        "an update must wait for the stop"
    );
    // The wrapper saves its agent and requests the restore.
    recovery::save_record(
        &manager.paths,
        &workspace.id,
        &plan.id,
        &shoal_protocol::executions::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    let request = manager
        .paths
        .space_state(&workspace.id)
        .join(format!("{}.restore-request.json", plan.id));
    std::fs::write(&request, "{}").unwrap();
    let finished = ExecutionEvent::Finished {
        exit_code: 143,
        memory: None,
    };
    protocol::write(connection.get_mut(), &finished)
        .await
        .unwrap();
    assert!(matches!(
        read(&mut connection).await,
        Control::Finished { complete: true }
    ));
    bounded(executing).await.unwrap().unwrap();
    bounded(stopping).await.unwrap();

    let notifications = manager.notifications.list(true, 10).await.unwrap();
    let stopped: Vec<_> = notifications
        .iter()
        .filter(|n| n.kind == AlertKind::SystemShutdown)
        .map(|n| n.message.as_str())
        .collect();
    assert_eq!(stopped.len(), 1, "{stopped:?}");
    // The stopped agent stays stopped work with its request, not a last session.
    let saved = Saved::load(&manager.paths, &workspace.id).unwrap();
    assert_eq!(saved.agents.len(), 2);
    assert!(saved.sessions.is_empty());
    assert!(request.exists());

    // Both restores queue, but the queue starts them only once the shutdown
    // is cancelled, the earlier first.
    let restores = async || {
        manager.agents.dispatch_queue(&*manager).await.unwrap();
        let entries = manager.agents.queue_entries(&*manager).await.unwrap();
        let state = |execution: &str| {
            let entry = entries
                .iter()
                .find(|entry| entry.execution.as_ref().map(ExecutionId::as_str) == Some(execution));
            entry.expect("the restore was not queued").state
        };
        (state("earlier"), state(plan.id.as_str()))
    };
    use crate::state::QueueState::{Queued, Starting};
    assert_eq!(restores().await, (Queued, Queued));
    manager.agents.stay(&*manager).await;
    assert!(!manager.agents.agent_queue().suspended());
    assert_eq!(restores().await.0, Starting);
}
