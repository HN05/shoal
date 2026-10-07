//! Connected agent metadata is deliberately transient: a daemon restart must
//! not turn an unknown execution into an automatic signal target.
use std::time::Instant;
use tokio::sync::watch;

use super::{GuardMode, Manager};
use crate::{daemon::notifications::NotificationKind, protocol::timing, state::WorkspaceState};

#[derive(Clone, Copy, PartialEq, Eq)]
enum StopReason {
    Overload,
    Pause,
}

pub(super) struct Agent {
    name: String,
    workspace: String,
    started: Instant,
    stop: watch::Sender<bool>,
    stop_reason: Option<StopReason>,
}

impl Agent {
    pub(super) fn cancel_recovery(&mut self) {
        self.stop_reason = None;
    }
}

impl Manager {
    pub(crate) async fn notify_agent_exit(
        &self,
        workspace: &crate::model::Workspace,
        agent: &str,
        exit_code: Option<i32>,
        complete: bool,
    ) {
        let message = match exit_code {
            Some(code) if complete => format!("{agent} exited with code {code}"),
            Some(code) => format!(
                "{agent} exited with code {code}, leaving processes behind; run shoal doctor"
            ),
            None => format!("{agent} disconnected without reporting; run shoal doctor"),
        };
        self.notify(
            Some(&workspace.name),
            NotificationKind::AgentExited,
            message,
        )
        .await;
    }

    pub(crate) async fn post_agent_exit(
        &self,
        workspace: &crate::model::Workspace,
        agent: &str,
        exit_code: Option<i32>,
        complete: bool,
    ) {
        if let Err(error) = self
            .run_agent_exit_hook(workspace, agent, exit_code, complete)
            .await
        {
            self.notify(
                Some(&workspace.name),
                NotificationKind::HookFailed,
                format!("agent exited; {error:#}"),
            )
            .await;
        }
        // A completion or forge transition may have happened while the agent
        // was running. Retry cleanup once the exit hook is finished.
        self.cleanup_notify.notify_one();
    }

    async fn run_agent_exit_hook(
        &self,
        workspace: &crate::model::Workspace,
        agent: &str,
        exit_code: Option<i32>,
        complete: bool,
    ) -> anyhow::Result<()> {
        use crate::hooks::{self, Hook, HookKind};
        if !self.agent_exit_workspace_ready(&workspace.id).await? {
            return Ok(());
        }
        let Some(command) = self
            .workspace_hook(workspace, HookKind::PostAgentExit)
            .await?
        else {
            return Ok(());
        };
        let _resources = self
            .resource_guard(&workspace.id, GuardMode::Exclusive)
            .await?;
        // Removal uses its own hooks; recheck after excluding new transitions.
        if !self.agent_exit_workspace_ready(&workspace.id).await? {
            return Ok(());
        }
        self.verify_worktree(workspace).await?;
        hooks::run_detached(
            Hook::PostAgentExit {
                agent,
                exit_code,
                complete,
            },
            workspace,
            &command,
            &self.paths,
        )
        .await
    }

    async fn agent_exit_workspace_ready(&self, id: &str) -> anyhow::Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(
                    db,
                    "SELECT 1 FROM workspaces WHERE id=?1 AND state=?2",
                    rusqlite::params![id, WorkspaceState::Ready],
                )
            })
            .await
    }

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
                    stop_reason: None,
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
        self.verify_agent_processes_stopped(&record).await?;
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

    async fn verify_agent_processes_stopped(
        &self,
        record: &crate::model::Execution,
    ) -> anyhow::Result<()> {
        await_process_proof(|| async {
            let ids = std::collections::HashSet::from([record.id.clone()]);
            let scan = crate::process::identity::scan(ids).await?;
            let processes = crate::process::execution::Processes::inspect(record, &scan).await?;
            Ok(scan.processes.is_empty()
                && processes.owned.is_empty()
                && processes.group_candidates.is_empty()
                && processes.visibility_complete())
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
        if agent.stop_reason != Some(StopReason::Overload) {
            return Ok(false);
        }
        stop.send(false)?;
        agent.stop_reason = None;
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
            .is_some_and(|agent| agent.stop_reason == Some(StopReason::Overload))
    }

    pub(crate) async fn agent_was_paused(&self, id: &str) -> bool {
        self.agents
            .lock()
            .await
            .get(id)
            .is_some_and(|agent| agent.stop_reason == Some(StopReason::Pause))
    }

    pub(crate) async fn pause_workspace_agents(
        &self,
        selector: &str,
        execution: Option<&str>,
    ) -> anyhow::Result<()> {
        let workspace = self.workspace(selector).await?;
        self.reserve_lifecycle(&workspace.id, WorkspaceState::Stopping)
            .await?;
        let result = async {
            self.verify_worktree(&workspace).await?;
            let selected = self.request_agent_pauses(&workspace.id, execution).await?;
            self.await_agent_pauses(&workspace.id, &selected).await
        }
        .await;
        self.set_state(
            &workspace.id,
            workspace.state,
            result
                .as_ref()
                .err()
                .map(|error| format!("{error:#}"))
                .or(workspace.error),
        )
        .await?;
        result
    }

    async fn request_agent_pauses(
        &self,
        workspace_id: &str,
        requested: Option<&str>,
    ) -> anyhow::Result<Vec<String>> {
        let executions = self.inspect_workspace(workspace_id).await?.executions;
        let selected = {
            let mut agents = self.agents.lock().await;
            let mut selected = Vec::new();
            for execution in executions {
                if requested.is_some_and(|id| id != execution.id) {
                    continue;
                }
                if let Some(agent) = agents.get_mut(&execution.id)
                    && !agent.stop.is_closed()
                    && (!*agent.stop.borrow() || agent.stop_reason.is_some())
                {
                    agent.stop_reason = Some(StopReason::Pause);
                    agent.stop.send(true)?;
                    selected.push((execution.id, agent.name.clone(), agent.workspace.clone()));
                }
            }
            selected
        };
        anyhow::ensure!(
            !selected.is_empty(),
            "no connected tracked agent matches; inspect the workspace or run shoal doctor"
        );
        for (_, name, workspace) in &selected {
            self.notify(
                Some(workspace),
                NotificationKind::AgentStopped,
                format!("Pausing {name}; restore with shoal resume; workspace and resource leases retained"),
            )
            .await;
        }
        Ok(selected.into_iter().map(|(id, _, _)| id).collect())
    }

    async fn await_agent_pauses(&self, workspace_id: &str, ids: &[String]) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + timing::WORKSPACE_STOP_TIMEOUT;
        loop {
            let executions = self.inspect_workspace(workspace_id).await?.executions;
            if !executions
                .iter()
                .any(|execution| ids.contains(&execution.id))
            {
                for id in ids {
                    anyhow::ensure!(
                        crate::execution::recovery::record_path(&self.paths, workspace_id, id)
                            .is_file(),
                        "agent {id} exited without a saved recovery record"
                    );
                }
                return Ok(());
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for agents to pause; inspect with shoal doctor"
            );
            tokio::time::sleep(timing::WORKSPACE_STOP_POLL_INTERVAL).await;
        }
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
                agent.stop_reason = Some(StopReason::Overload);
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
            format!("Stopping {name}: {reason}; workspace and resource leases retained"),
        )
        .await;
        true
    }
}

async fn await_process_proof<F>(mut probe: impl FnMut() -> F) -> anyhow::Result<()>
where
    F: std::future::Future<Output = anyhow::Result<bool>>,
{
    let deadline = tokio::time::Instant::now() + timing::WORKSPACE_STOP_TIMEOUT;
    loop {
        if probe().await? {
            return Ok(());
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "cannot resume an execution whose processes are not proven stopped"
        );
        tokio::time::sleep(timing::WORKSPACE_STOP_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod process_proof_tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn transient_incomplete_visibility_is_rechecked() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        await_process_proof(|| async {
            Ok(attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 1)
        })
        .await
        .unwrap();
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn persistent_incomplete_visibility_still_refuses_recovery() {
        let started = tokio::time::Instant::now();
        let error = await_process_proof(|| async { Ok(false) })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not proven stopped"));
        assert_eq!(
            tokio::time::Instant::now() - started,
            timing::WORKSPACE_STOP_TIMEOUT
        );
        let error = await_process_proof(|| async { anyhow::bail!("inventory failed") })
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "inventory failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        daemon::{ports::PortRequest, workspace::ExecutionKind},
        test_support::{manager, repository},
    };
    use futures_util::FutureExt;

    #[tokio::test]
    async fn agent_exit_hook_reports_disconnects_and_skips_removing_workspaces() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "agent".into(), None, None, None)
            .await
            .unwrap();
        let hook = workspace.path.join("exited");
        fs::write(&hook, "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$SHOAL_AGENT\" \"$SHOAL_AGENT_EXIT_CODE\" \"$SHOAL_AGENT_EXIT_COMPLETE\" >> exits\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        manager
            .set_repository_config(&repo.id, Some("post_agent_exit_cmd = 'exited'\n".into()))
            .await
            .unwrap();
        let cleanup_woken = manager.cleanup_notify.notified();
        manager
            .notify_agent_exit(&workspace, "helper", None, false)
            .await;
        manager
            .post_agent_exit(&workspace, "helper", None, false)
            .await;
        assert!(cleanup_woken.now_or_never().is_some());
        assert_eq!(
            fs::read_to_string(workspace.path.join("exits")).unwrap(),
            "helper||false\n"
        );
        assert!(manager.completion(&workspace.id).await.unwrap().is_none());
        manager
            .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
            .await
            .unwrap();
        manager
            .notify_agent_exit(&workspace, "helper", Some(143), true)
            .await;
        manager
            .post_agent_exit(&workspace, "helper", Some(143), true)
            .await;
        assert_eq!(
            fs::read_to_string(workspace.path.join("exits")).unwrap(),
            "helper||false\n"
        );
        let events = manager.notifications(false, 10).await.unwrap();
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|event| event.kind == NotificationKind::AgentExited)
        );
    }

    #[tokio::test]
    async fn agent_exit_wakes_cleanup_for_completed_workspace() {
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo");
        let repo = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "agent".into(), None, None, None)
            .await
            .unwrap();
        let execution = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
            .unwrap();
        manager.mark_done(&workspace.id, Some(true)).await.unwrap();
        assert!(manager.cleanup_notify.notified().now_or_never().is_some());
        let complete = manager
            .finish_execution(execution.plan.id, ExecutionKind::Command, Some(0))
            .await
            .unwrap();
        assert!(complete);
        manager
            .post_agent_exit(&workspace, "helper", Some(0), complete)
            .await;
        assert!(manager.cleanup_notify.notified().now_or_never().is_some());
        crate::daemon::cleanup::sweep(&manager, &mut Default::default())
            .await
            .unwrap();
        assert!(!workspace.path.exists());
        assert!(manager.workspace(&workspace.id).await.is_err());
    }

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
        let mut pending = {
            let manager = manager.clone();
            let id = workspace.id;
            Box::pin(async move { manager.await_overload_recovery(&id).await })
        };
        manager.recovery_ready.send_replace(Some(0));
        crate::test_support::assert_pending(&mut pending).await;
        manager.recovery_ready.send_replace(Some(1));
        tokio::time::timeout(std::time::Duration::from_secs(60), pending)
            .await
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
