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
    overloaded: bool,
}

impl Agent {
    pub(super) fn cancel_recovery(&mut self) {
        self.overloaded = false;
    }
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
                    overloaded: false,
                },
            );
        }
    }

    pub(crate) async fn pause_agent_execution(
        &self,
        id: &str,
        workspace_id: &str,
    ) -> anyhow::Result<()> {
        let record = self
            .inspect_workspace(workspace_id)
            .await?
            .executions
            .into_iter()
            .find(|execution| execution.id == id)
            .ok_or_else(|| anyhow::anyhow!("execution no longer exists"))?;
        let scan = crate::process::identity::scan(std::collections::HashSet::from([id.to_owned()]))
            .await?;
        let processes = crate::process::execution::Processes::inspect(&record, &scan).await?;
        anyhow::ensure!(
            scan.processes.is_empty()
                && processes.owned.is_empty()
                && processes.group_candidates.is_empty()
                && processes.visibility_complete(),
            "cannot resume an execution whose processes are not proven stopped"
        );
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE executions SET child=NULL,group_id=NULL WHERE id=?1",
                    [id],
                )?;
                Ok(())
            })
            .await
    }

    pub(crate) async fn resume_agent_execution(&self, id: &str) -> anyhow::Result<bool> {
        let connections = self.connections.lock().await;
        let stop = connections
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("execution disconnected"))?;
        let mut agents = self.agents.lock().await;
        let agent = agents
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("agent no longer exists"))?;
        if !agent.overloaded {
            return Ok(false);
        }
        stop.send(false)?;
        agent.overloaded = false;
        agent.started = Instant::now();
        let (name, workspace) = (agent.name.clone(), agent.workspace.clone());
        drop(agents);
        drop(connections);
        self.notify(
            Some(&workspace),
            NotificationKind::AgentResumed,
            format!("Restoring {name} after sustained healthy system load"),
        )
        .await;
        Ok(true)
    }

    pub(crate) async fn agent_was_overloaded(&self, id: &str) -> bool {
        self.agents
            .lock()
            .await
            .get(id)
            .is_some_and(|agent| agent.overloaded)
    }

    pub(crate) fn reset_overload_recovery(&self) {
        self.recovery_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.recovery_ready.send_replace(None);
    }

    pub(crate) async fn await_overload_recovery(&self, workspace_id: &str) -> anyhow::Result<()> {
        let _gate = self.recovery_gate.lock().await;
        let mut ready = self.recovery_ready.subscribe();
        loop {
            let epoch = self
                .recovery_epoch
                .load(std::sync::atomic::Ordering::Relaxed);
            if *ready.borrow_and_update() != Some(epoch) {
                ready.changed().await?;
                continue;
            }
            let workspace = self.workspace(workspace_id).await?;
            self.verify_worktree(&workspace).await?;
            anyhow::ensure!(
                workspace.state == crate::state::WorkspaceState::Ready,
                "workspace is no longer ready for agent recovery"
            );
            // Ownership checks may yield while load worsens; stale permission
            // cannot authorize a restore after those checks finish.
            if self
                .recovery_epoch
                .load(std::sync::atomic::Ordering::Relaxed)
                != epoch
                || *ready.borrow_and_update() != Some(epoch)
            {
                continue;
            }
            self.reset_overload_recovery();
            break;
        }
        Ok(())
    }

    pub(crate) async fn stop_agent_for_overload(&self, reason: &str) -> bool {
        let stopped = {
            let mut agents = self.agents.lock().await;
            let selected = agents
                .values_mut()
                .filter(|agent| !agent.stop.is_closed() && !*agent.stop.borrow())
                .max_by_key(|agent| agent.started);
            selected.and_then(|agent| {
                agent.stop.send(true).ok()?;
                agent.overloaded = true;
                Some((agent.name.clone(), agent.workspace.clone()))
            })
        };
        let Some((name, workspace)) = stopped else {
            return false;
        };
        self.reset_overload_recovery();
        self.notify(
            Some(&workspace),
            NotificationKind::AgentStopped,
            format!("Stopping {name}: {reason}; workspace and resource leases retained. Restore with shoal resume {workspace} after it stops"),
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

#[cfg(test)]
mod recovery_tests {
    use crate::test_support::{manager, repository};

    #[tokio::test]
    async fn each_restore_consumes_a_fresh_healthy_interval() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "resume".into(), None, None, None)
            .await
            .unwrap();
        manager.recovery_ready.send_replace(Some(0));
        manager
            .await_overload_recovery(&workspace.id)
            .await
            .unwrap();
        assert_eq!(
            manager
                .recovery_epoch
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        let pending = {
            let manager = manager.clone();
            let id = workspace.id;
            tokio::spawn(async move { manager.await_overload_recovery(&id).await })
        };
        manager.recovery_ready.send_replace(Some(0));
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!pending.is_finished());
        manager.recovery_ready.send_replace(Some(1));
        tokio::time::timeout(std::time::Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            manager
                .recovery_epoch
                .load(std::sync::atomic::Ordering::Relaxed),
            2
        );
    }
}
