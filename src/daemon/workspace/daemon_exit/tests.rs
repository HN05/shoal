use std::{sync::Arc, time::Duration};

use tokio::{io::BufReader, net::UnixStream, task::JoinHandle};

use super::*;
use crate::{
    daemon::{
        execution_connection::{ExecutionContext, execute},
        notifications::NotificationKind,
        workspace::ExecutionKind,
    },
    execution::recovery,
    model::Workspace,
    protocol::{self, Body, Control, ExecutionEvent, Response},
    test_support::{manager, repository},
};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("the daemon's exit stalled")
}

async fn read<T: serde::de::DeserializeOwned>(stream: &mut BufReader<UnixStream>) -> T {
    bounded(protocol::read_buffered(stream)).await.unwrap()
}

/// A workspace running a connected command, and an agent stopped earlier
/// whose restore waits in the queue.
struct Running {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    workspace: Workspace,
    connection: BufReader<UnixStream>,
    executing: JoinHandle<anyhow::Result<()>>,
}

impl Running {
    async fn start() -> Self {
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
        assert!(
            matches!(response.body, Body::Execution(_)),
            "execution was not registered"
        );
        let started = ExecutionEvent::Started {
            child: None,
            group_id: wrapper.pid,
            restored: false,
        };
        protocol::write(connection.get_mut(), &started)
            .await
            .unwrap();
        assert!(matches!(read(&mut connection).await, Control::Started));
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
        Self {
            _root: root,
            manager,
            workspace,
            connection,
            executing,
        }
    }

    /// End the daemon's work with `ending`, answering the stop it sends,
    /// which the returned control was.
    async fn end(mut self, ending: Ending) -> (Self, Control) {
        let ending = tokio::spawn({
            let manager = self.manager.clone();
            async move { manager.end_work(ending).await }
        });
        let control: Control = read(&mut self.connection).await;
        let finished = ExecutionEvent::Finished { exit_code: 143 };
        protocol::write(self.connection.get_mut(), &finished)
            .await
            .unwrap();
        assert!(matches!(
            read(&mut self.connection).await,
            Control::Finished { complete: true }
        ));
        bounded(&mut self.executing).await.unwrap().unwrap();
        bounded(ending).await.unwrap();
        (self, control)
    }

    async fn queued_restore(&self) -> bool {
        self.manager
            .queued_stopped_restores()
            .await
            .unwrap()
            .contains("earlier")
    }

    async fn notifications(&self, kind: NotificationKind) -> Vec<String> {
        let notifications = self.manager.notifications(true, 10).await.unwrap();
        notifications
            .into_iter()
            .filter(|notification| notification.kind == kind)
            .map(|notification| notification.message)
            .collect()
    }
}

#[tokio::test]
async fn a_signal_stops_work_saved_first_keeping_its_restores() {
    let (running, control) = Running::start().await.end(Ending::Save).await;
    assert!(
        matches!(
            &control,
            Control::Pause { reason: Some(reason), removed: false, shutdown: true }
                if reason == "the Shoal daemon is stopping"
        ),
        "{control:?}"
    );
    assert!(running.queued_restore().await);
    assert_eq!(
        running.notifications(NotificationKind::DaemonStopped).await,
        [
            "Stopped work in 1 workspace before the Shoal daemon stopped; the agent queue restores its agents"
        ]
    );
}

#[tokio::test]
async fn daemon_stop_stops_work_as_stop_all_does() {
    let (running, control) = Running::start().await.end(Ending::Stop).await;
    assert!(
        matches!(
            &control,
            Control::Pause { reason: Some(reason), removed: false, shutdown: false }
                if reason == "stopped by shoal stop"
        ),
        "{control:?}"
    );
    // Agents stay down until `shoal resume`.
    assert!(!running.queued_restore().await);
    let saved = recovery::Saved::load(&running.manager.paths, &running.workspace.id).unwrap();
    assert!(saved.agents.is_empty(), "{:?}", saved.agents);
    assert!(
        running
            .notifications(NotificationKind::DaemonStopped)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_restart_and_a_signal_after_a_system_shutdown_stop_leave_work_alone() {
    use futures_util::FutureExt;
    let mut running = Running::start().await;
    bounded(running.manager.end_work(Ending::Detach)).await;
    // A signal during a system shutdown's stop waits for that stop.
    let mut stopping = running.manager.shutdown_stopped.lock().await;
    let mut signaled = Box::pin(running.manager.end_work(Ending::Save));
    assert!(signaled.as_mut().now_or_never().is_none());
    *stopping = true;
    drop(stopping);
    bounded(signaled).await;
    assert!(running.queued_restore().await);
    // The connection still runs: the next control answers its exit.
    let finished = ExecutionEvent::Finished { exit_code: 0 };
    protocol::write(running.connection.get_mut(), &finished)
        .await
        .unwrap();
    assert!(matches!(
        read(&mut running.connection).await,
        Control::Finished { complete: true }
    ));
    bounded(&mut running.executing).await.unwrap().unwrap();
    assert!(
        running
            .notifications(NotificationKind::DaemonStopped)
            .await
            .is_empty()
    );
}
