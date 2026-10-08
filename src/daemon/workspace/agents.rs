//! Connected agent metadata is deliberately transient: a daemon restart must
//! not turn an unknown execution into an automatic signal target.
use std::time::Instant;
use tokio::sync::watch;

use super::{GuardMode, Manager};
use crate::{daemon::notifications::NotificationKind, protocol::timing, state::WorkspaceState};

pub(super) struct Agent {
    name: String,
    workspace: String,
    workspace_id: String,
    started: Instant,
    recover: bool,
    stop: watch::Sender<bool>,
    /// The pressure reason while an overload stop awaits automatic recovery.
    overload: Option<String>,
}

impl Agent {
    pub(super) fn cancel_recovery(&mut self) {
        self.overload = None;
    }
}

impl Manager {
    pub(crate) async fn notify_agent_exit(
        &self,
        workspace: &crate::model::Workspace,
        agent: &str,
        exit_code: Option<i32>,
        complete: bool,
        overload: Option<(&str, &str)>,
    ) {
        let message = if let Some((execution_id, reason)) = overload {
            let status = exit_code.map_or_else(
                || "without an exit code".into(),
                |code| format!("with code {code}"),
            );
            let recovery = if crate::execution::recovery::record_path(
                &self.paths,
                &workspace.id,
                execution_id,
            )
            .is_file()
            {
                format!(
                    "restore with shoal resume {} --execution {execution_id}",
                    workspace.name
                )
            } else {
                "recovery record unavailable; inspect the workspace and relaunch the agent's saved session".into()
            };
            let reconcile = if complete {
                ""
            } else {
                "; execution remains active or unknown; run shoal doctor before resuming"
            };
            format!(
                "{agent} stopped after {reason} ({status}); {recovery}{reconcile}; workspace and resource leases retained"
            )
        } else {
            match exit_code {
                Some(code) if complete => format!("{agent} exited with code {code}"),
                Some(code) => format!(
                    "{agent} exited with code {code}, leaving processes behind; run shoal doctor"
                ),
                None => format!("{agent} disconnected without reporting; run shoal doctor"),
            }
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

    pub(crate) async fn track_agent(
        &self,
        id: &str,
        name: &str,
        workspace_id: &str,
        workspace: &str,
        recover: bool,
    ) {
        let connections = self.connections.lock().await;
        if let Some(stop) = connections.get(id) {
            self.agents.lock().await.insert(
                id.into(),
                Agent {
                    name: name.into(),
                    workspace: workspace.into(),
                    workspace_id: workspace_id.into(),
                    started: Instant::now(),
                    recover,
                    stop: stop.clone(),
                    overload: None,
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
        if agent.overload.is_none() {
            return Ok(false);
        }
        stop.send(false)?;
        agent.overload = None;
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

    pub(crate) async fn agent_overload_reason(&self, id: &str) -> Option<String> {
        self.agents
            .lock()
            .await
            .get(id)
            .and_then(|agent| agent.overload.clone())
    }

    pub(crate) async fn agent_was_overloaded(&self, id: &str) -> bool {
        self.agent_overload_reason(id).await.is_some()
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
                .iter_mut()
                .filter(|(_, agent)| !agent.stop.is_closed() && !*agent.stop.borrow())
                .max_by_key(|(_, agent)| agent.started);
            selected.and_then(|(id, agent)| {
                let handoff = crate::execution::recovery::save_record(
                    &self.paths,
                    &agent.workspace_id,
                    id,
                    &agent.name,
                    Some(reason),
                );
                if let Err(error) = &handoff {
                    eprintln!("overload recovery handoff not saved: {error:#}");
                }
                agent.overload = Some(reason.into());
                agent.stop.send(true).ok()?;
                Some((
                    id.clone(),
                    agent.name.clone(),
                    agent.workspace.clone(),
                    agent.recover,
                    handoff.is_ok(),
                ))
            })
        };
        let Some((id, name, workspace, recover, handoff_saved)) = stopped else {
            return false;
        };
        self.reset_overload_recovery();
        let recovery = if !recover {
            "automatic recovery unavailable: configure [agent_resume] with a session restore command"
        } else if !self.config().overload.recovery.enabled {
            "automatic recovery disabled by overload.recovery.enabled"
        } else {
            "automatic session restore waits for healthy load"
        };
        self.notify(
            Some(&workspace),
            NotificationKind::AgentStopped,
            format!("Stopping {name}: {reason}; {recovery}; after the wrapper exits, restore with shoal resume {workspace} --execution {id}{}; workspace and resource leases retained", if handoff_saved { "" } else { "; recovery handoff could not be saved; run shoal doctor if the wrapper exits" }),
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
            .notify_agent_exit(&workspace, "helper", None, false, None)
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
            .notify_agent_exit(&workspace, "helper", Some(143), true, None)
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
    async fn overload_notifications_explain_recovery_policy_and_missing_records() {
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
        for (recover, enabled, expected) in [
            (false, true, "configure [agent_resume]"),
            (true, false, "automatic recovery disabled"),
            (
                true,
                true,
                "automatic session restore waits for healthy load",
            ),
        ] {
            let mut config = crate::config::Config::default();
            config.overload.recovery.enabled = enabled;
            manager.publish_config(config);
            let execution = manager
                .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
                .await
                .unwrap();
            let id = &execution.plan.id;
            manager
                .track_agent(id, "codex", &workspace.id, &workspace.name, recover)
                .await;
            assert!(
                manager
                    .stop_agent_for_overload("critical memory pressure")
                    .await
            );
            let record = crate::execution::recovery::record_path(&manager.paths, &workspace.id, id);
            assert!(record.is_file());
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&std::fs::read(&record).unwrap())
                    .unwrap(),
                serde_json::json!({"agent":"codex", "stop_reason":"critical memory pressure"})
            );
            std::fs::remove_file(&record).unwrap();
            let events = manager.notifications(false, 1).await.unwrap();
            assert!(
                events[0].message.contains(expected),
                "{}",
                events[0].message
            );
            assert!(
                events[0]
                    .message
                    .contains(&format!("shoal resume {} --execution {id}", workspace.name))
            );
            assert_eq!(
                manager.agent_overload_reason(id).await.as_deref(),
                Some("critical memory pressure")
            );
            manager
                .notify_agent_exit(
                    &workspace,
                    "codex",
                    Some(143),
                    false,
                    Some((id, "critical memory pressure")),
                )
                .await;
            let events = manager.notifications(false, 1).await.unwrap();
            assert!(events[0].message.contains("critical memory pressure"));
            assert!(events[0].message.contains("recovery record unavailable"));
            assert!(events[0].message.contains("shoal doctor before resuming"));
            manager
                .finish_execution(id.clone(), ExecutionKind::Command, Some(143))
                .await
                .unwrap();
        }
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
            .track_agent(
                &first.plan.id,
                "first",
                &workspace.id,
                &workspace.name,
                false,
            )
            .await;
        let second = manager
            .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
            .await
            .unwrap();
        manager
            .track_agent(
                &second.plan.id,
                "second",
                &workspace.id,
                &workspace.name,
                false,
            )
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
