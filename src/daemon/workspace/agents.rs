//! Connected agent metadata is deliberately transient: a daemon restart must
//! not turn an unknown execution into an automatic signal target.
use std::time::Instant;
use tokio::sync::watch;

use super::Manager;
use crate::daemon::notifications::NotificationKind;

pub(super) struct Agent {
    name: String,
    workspace: String,
    started: Instant,
    stop: watch::Sender<bool>,
}

impl Manager {
    pub(crate) async fn track_agent(&self, id: &str, name: &str, workspace: &str) {
        let connections = self.connections.lock().await;
        if let Some(stop) = connections.get(id) {
            self.agents.lock().await.insert(
                id.into(),
                Agent {
                    name: name.into(),
                    workspace: workspace.into(),
                    started: Instant::now(),
                    stop: stop.clone(),
                },
            );
        }
    }

    pub(crate) async fn stop_agent_for_overload(&self, reason: &str) -> bool {
        let stopped = {
            let agents = self.agents.lock().await;
            let selected = agents
                .values()
                .filter(|agent| !agent.stop.is_closed() && !*agent.stop.borrow())
                .max_by_key(|agent| agent.started);
            selected.and_then(|agent| {
                agent.stop.send(true).ok()?;
                Some((agent.name.clone(), agent.workspace.clone()))
            })
        };
        let Some((name, workspace)) = stopped else {
            return false;
        };
        self.notify(
            Some(&workspace),
            NotificationKind::AgentStopped,
            format!("Stopping {name}: {reason}; workspace and resource leases retained"),
        )
        .await;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        daemon::{ports::PortRequest, workspace::ExecutionKind},
        test_support::{manager, repository},
    };

    #[tokio::test]
    async fn overload_stops_only_one_agent_and_preserves_work_and_leases() {
        let (_root, manager) = manager().await;
        let repo = repository(_root.path(), "repo");
        let repo = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "agent".into(), None, None, None)
            .await
            .unwrap();
        manager
            .acquire_port(&workspace.id, "web".into(), PortRequest::default(), None)
            .await
            .unwrap();
        std::fs::write(workspace.path.join("unfinished"), "work in progress").unwrap();
        let command = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
            .unwrap();
        let first = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
            .unwrap();
        manager
            .track_agent(&first.plan.id, "first", &workspace.name)
            .await;
        let second = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
            .unwrap();
        manager
            .track_agent(&second.plan.id, "second", &workspace.name)
            .await;
        assert!(
            manager
                .stop_agent_for_overload("critical memory pressure")
                .await
        );
        assert!(*second.stop.borrow());
        assert!(!*first.stop.borrow());
        assert!(!*command.stop.borrow());
        let retained = manager.inspect_workspace(&workspace.id).await.unwrap();
        assert_eq!(retained.workspace.state, workspace.state);
        assert_eq!(retained.ports.len(), 1);
        assert_eq!(retained.executions.len(), 3);
        assert_eq!(
            std::fs::read_to_string(workspace.path.join("unfinished")).unwrap(),
            "work in progress"
        );
        let notifications = manager.notifications(false, 10).await.unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].kind, NotificationKind::AgentStopped);
        assert!(
            notifications[0]
                .message
                .contains("second: critical memory pressure")
        );
        // Already stopping and disconnected agents cannot be selected again.
        drop(first.stop);
        assert!(
            !manager
                .stop_agent_for_overload("critical memory pressure")
                .await
        );
        manager
            .finish_execution(second.plan.id, ExecutionKind::Command, Some(1))
            .await
            .unwrap();
        assert_eq!(manager.agents.lock().await.len(), 1);
    }
}
