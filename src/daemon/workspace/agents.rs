//! Connected agent metadata is deliberately transient: a daemon restart must
//! not turn an unknown execution into an automatic signal target.
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::{OwnedRwLockReadGuard, watch};

use super::{GuardMode, Manager, agent_queue::Grant, agent_turns::Turn, capacity::Activity};
pub(crate) use crate::protocol::Protection;

use crate::daemon::log;
use crate::{
    daemon::notifications::NotificationKind,
    protocol::timing,
    state::{AgentState, WorkspaceState},
};

mod restart;

pub(super) struct Agent {
    name: String,
    workspace: String,
    workspace_id: String,
    started: Instant,
    recover: bool,
    stop: watch::Sender<bool>,
    /// Set while an overload stop awaits automatic recovery.
    overload: Option<Protection>,
    /// Holds back a daemon update from the stop until the execution's
    /// connection takes over, so an update waits for the restore.
    update_hold: Option<OwnedRwLockReadGuard<()>>,
    /// Set while `shoal restart` restarts the agent.
    restart: Option<restart::Restart>,
    /// The turn state the agent reported last, which the workspace's state
    /// keeps only while no other execution there reports.
    turn: Option<crate::state::AgentState>,
}

impl Agent {
    pub(super) fn cancel_recovery(&mut self) {
        self.overload = None;
        self.update_hold = None;
        self.restart = None;
    }

    /// Whether protection stopped the agent and it awaits automatic recovery.
    pub(super) fn protected(&self) -> bool {
        self.overload.is_some()
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
                "; its processes are not proven stopped, so the execution stays unknown until they exit or shoal stop stops them"
            };
            format!(
                "{agent} stopped after {reason} ({status}); {recovery}{reconcile}; workspace and resource leases retained"
            )
        } else {
            match exit_code {
                Some(code) if complete => format!("{agent} exited with code {code}"),
                Some(code) => format!(
                    "{agent} exited with code {code}, leaving processes behind; stop them with shoal stop {}",
                    workspace.name
                ),
                None => format!(
                    "{agent} disconnected without reporting; stop what remains of it with shoal stop {}",
                    workspace.name
                ),
            }
        };
        self.notify(
            Some(&workspace.name),
            NotificationKind::AgentExited,
            message,
        )
        .await;
    }

    /// Forget a finished agent's reported session, first keeping it as the
    /// workspace's last session when the agent `exited` on its own before the
    /// assignment finished, for `shoal resume` to continue.
    pub(crate) async fn end_agent_session(
        &self,
        workspace: &crate::model::Workspace,
        agent: &str,
        execution: &str,
        exited: bool,
    ) {
        use crate::execution::recovery::{self, LastSession};
        let kept = async {
            if exited && self.completion(&workspace.id).await?.is_none() {
                LastSession::save(&self.paths, &workspace.id, agent, execution)?;
            }
            recovery::forget_session(&self.paths, &workspace.id, execution)
        }
        .await;
        if let Err(error) = kept {
            log!("agent session of {execution} not kept: {error:#}");
        }
    }

    /// A new agent or a finished assignment replaces the last session.
    pub(crate) fn forget_last_session(&self, workspace_id: &str) {
        let forgotten = crate::execution::recovery::LastSession::forget(&self.paths, workspace_id);
        if let Err(error) = forgotten {
            log!("last agent session of {workspace_id} not forgotten: {error:#}");
        }
    }

    pub(crate) async fn post_agent_exit(
        &self,
        workspace: &crate::model::Workspace,
        agent: &str,
        exit_code: Option<i32>,
        complete: bool,
    ) {
        // Processes that are not proven stopped may still write; removal or
        // the next checkpoint keeps what they leave.
        if complete {
            self.checkpoint_after_agent(&workspace.id).await;
        }
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
        let configured_env = self.configured_workspace_environment(workspace).await?;
        hooks::run_detached(
            Hook::PostAgentExit {
                agent,
                exit_code,
                complete,
            },
            workspace,
            &command,
            &self.paths,
            &configured_env,
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

    /// `running` is how long the agent has already run, so a reattached
    /// agent keeps its place in the newest-first overload order.
    pub(crate) async fn track_agent(
        &self,
        id: &str,
        name: &str,
        workspace_id: &str,
        workspace: &str,
        recover: bool,
        running: Duration,
    ) {
        let connections = self.connections.lock().await;
        if let Some(stop) = connections.get(id) {
            self.agents.lock().await.insert(
                id.into(),
                Agent {
                    name: name.into(),
                    workspace: workspace.into(),
                    workspace_id: workspace_id.into(),
                    started: Instant::now()
                        .checked_sub(running)
                        .unwrap_or_else(Instant::now),
                    recover,
                    stop: stop.clone(),
                    overload: None,
                    update_hold: None,
                    restart: None,
                    turn: None,
                },
            );
        }
    }

    /// Prove that a protection-stopped agent ended, then forget its stopped
    /// child. Identity-verified processes it left outside its process group,
    /// such as background shell commands, are stopped as `shoal stop`
    /// stops them; the waiting wrapper is not a target. Reports what survives
    /// when `patience` passes without proof.
    pub(crate) async fn pause_agent_execution(
        &self,
        id: &str,
        workspace_id: &str,
        patience: Duration,
    ) -> anyhow::Result<Pause> {
        let record = self
            .workspace_executions(workspace_id)
            .await?
            .into_iter()
            .find(|execution| execution.id == id)
            .ok_or_else(|| anyhow::anyhow!("execution no longer exists"))?;
        if let Some(issue) =
            await_process_proof(patience, || self.stop_agent_survivors(&record)).await
        {
            return Ok(Pause::Unproven(issue));
        }
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE executions SET child=NULL,group_id=NULL WHERE id=?1",
                    [id],
                )?;
                Ok(())
            })
            .await?;
        Ok(Pause::Paused)
    }

    /// Stop what an agent that reported its exit left running outside its
    /// process group, such as background shell commands, so nothing outlives
    /// it. The completion scan that follows decides whether anything survived.
    pub(crate) async fn stop_exited_agent_processes(&self, id: &str) {
        let query_id = id.to_owned();
        let stopped = async {
            let record = self
                .store
                .run(move |db| crate::daemon::store::find_execution(db, &query_id))
                .await?
                .ok_or_else(|| anyhow::anyhow!("execution no longer exists"))?;
            inspect_processes(&record).await?.stop_owned().await
        };
        if let Err(error) = stopped.await {
            log!("processes of exited agent {id} not stopped: {error:#}");
        }
    }

    /// Stop the execution's verified survivors and report what keeps its
    /// processes from being proven stopped, if anything does.
    async fn stop_agent_survivors(&self, record: &crate::model::Execution) -> Option<String> {
        let processes = match inspect_processes(record).await {
            Ok(processes) => processes,
            Err(error) => return Some(format!("process inspection failed: {error:#}")),
        };
        if let Some(issue) = processes.completion_issue() {
            // A survivor can fork while stopping, so only a later scan proves the stop.
            return Some(match processes.stop_owned().await {
                Ok(()) => issue,
                Err(error) => format!("{issue}; {error:#}"),
            });
        }
        // The daemon forgets a stopped child only after proving the stop, so
        // a reattached waiting wrapper's record needs no launch identity.
        let proven_before =
            record.wrapper.is_some() && record.child.is_none() && record.group_id.is_none();
        (!processes.visibility_complete() && !proven_before)
            .then(|| "its launch identity is incomplete".into())
    }

    /// Tell the user why a protection-stopped agent's automatic restore waits
    /// beyond healthy readings.
    pub(crate) async fn notify_restore_waits(&self, id: &str, issue: &str) {
        let Some((name, workspace)) = self
            .agents
            .lock()
            .await
            .get(id)
            .map(|agent| (agent.name.clone(), agent.workspace.clone()))
        else {
            return;
        };
        log!("restore of {name} in {workspace} waits for its processes to stop: {issue}");
        self.notify(
            Some(&workspace),
            NotificationKind::RecoveryBlocked,
            format!("Automatic restore of {name} waits until its remaining processes exit: {issue}; shoal stop {workspace} cancels the restore"),
        )
        .await;
    }

    /// Resume the stopped agent `id`, which `grant` admitted when its
    /// automatic restore did.
    pub(crate) async fn resume_agent_execution(
        &self,
        id: &str,
        grant: Option<Grant>,
    ) -> anyhow::Result<bool> {
        let connections = self.connections.lock().await;
        let stop = connections
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("execution disconnected"))?;
        let mut agents = self.agents.lock().await;
        let agent = agents
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("agent no longer exists"))?;
        let restarting = agent
            .restart
            .as_mut()
            .map(restart::Restart::resume)
            .is_some();
        if agent.overload.is_none() && !restarting {
            return Ok(false);
        }
        stop.send(false)?;
        agent.overload = None;
        agent.update_hold = None;
        agent.started = Instant::now();
        // The resumed agent reports its own turns.
        agent.turn = None;
        let (name, workspace) = (agent.name.clone(), agent.workspace.clone());
        drop(agents);
        drop(connections);
        // `shoal restart` reports its own restarts.
        if restarting {
            return Ok(true);
        }
        let cause = match grant {
            Some(Grant::Update) => "for an update",
            _ => "after sustained healthy readings",
        };
        self.notify(
            Some(&workspace),
            NotificationKind::AgentResumed,
            format!("Restoring {name} {cause}"),
        )
        .await;
        Ok(true)
    }

    /// Restore the protection stop of an agent whose wrapper reattached
    /// while waiting for its automatic restore. A restart continues without
    /// the request that asked for it, which ended with the previous daemon.
    pub(crate) async fn await_agent_restore(&self, id: &str, protection: Protection) {
        if let Some(agent) = self.agents.lock().await.get_mut(id) {
            if protection.is_restart() {
                agent.restart = Some(restart::Restart::unanswered());
            } else {
                agent.overload = Some(protection);
            }
        }
    }

    pub(crate) async fn agent_overload(&self, id: &str) -> Option<Protection> {
        self.agents
            .lock()
            .await
            .get(id)
            .and_then(|agent| agent.overload.clone())
    }

    /// The guard a protection stop took for the execution's connection.
    pub(crate) async fn take_update_hold(&self, id: &str) -> Option<OwnedRwLockReadGuard<()>> {
        self.agents
            .lock()
            .await
            .get_mut(id)
            .and_then(|agent| agent.update_hold.take())
    }

    pub(crate) async fn agent_was_overloaded(&self, id: &str) -> bool {
        self.agent_overload(id).await.is_some()
    }

    pub(crate) fn reset_overload_recovery(&self) {
        self.recovery_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.recovery_ready.send_replace(None);
    }

    /// The recovery epoch the overload monitor published sustained healthy
    /// readings for, if it still holds.
    pub(super) fn published_recovery(&self) -> Option<u64> {
        let epoch = self
            .recovery_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        (*self.recovery_ready.borrow() == Some(epoch)).then_some(epoch)
    }

    /// Consume the healthy interval published for `epoch`, returning the
    /// epoch a restore admitted on it holds; `None` when a stop, reload or
    /// unsafe reading advanced the epoch first.
    pub(super) fn consume_recovery(&self, epoch: u64) -> Option<u64> {
        self.recovery_epoch
            .compare_exchange(
                epoch,
                epoch + 1,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .ok()?;
        self.recovery_ready.send_replace(None);
        Some(epoch + 1)
    }

    /// Queue the automatic restore of the protection-stopped agent `id`.
    pub(crate) async fn queue_agent_restore(&self, id: &str) -> anyhow::Result<()> {
        let (name, workspace_id) = self
            .agents
            .lock()
            .await
            .get(id)
            .map(|agent| (agent.name.clone(), agent.workspace_id.clone()))
            .ok_or_else(|| anyhow::anyhow!("agent no longer exists"))?;
        self.queue_restore(id, &name, &workspace_id).await
    }

    /// Wait until the agent queue admits the restore of `id` and the
    /// admission holds through the checks of its workspace.
    pub(crate) async fn admit_agent_restore(
        &self,
        id: &str,
        workspace_id: &str,
    ) -> anyhow::Result<Grant> {
        loop {
            let grant = self.await_restore_admission(id).await?;
            if self.confirm_restore(id, workspace_id, grant).await? {
                return Ok(grant);
            }
        }
    }

    /// Check that a stopped agent's workspace is ready and still the
    /// recorded worktree, so a replaced one never receives the agent.
    pub(crate) async fn verify_restore_workspace(&self, workspace_id: &str) -> anyhow::Result<()> {
        let workspace = self.workspace(workspace_id).await?;
        self.verify_worktree(&workspace).await?;
        anyhow::ensure!(
            workspace.state == crate::state::WorkspaceState::Ready,
            "workspace is no longer ready for agent recovery"
        );
        Ok(())
    }

    /// Check that an admitted restore's workspace is ready and verified. A
    /// stop or reload since healthy readings admitted it revokes it, since
    /// they no longer hold, and returns it to its place; an update's admission
    /// holds.
    pub(super) async fn confirm_restore(
        &self,
        id: &str,
        workspace_id: &str,
        grant: Grant,
    ) -> anyhow::Result<bool> {
        self.verify_restore_workspace(workspace_id).await?;
        let current = match grant {
            Grant::Update => true,
            Grant::Recovery(epoch) => {
                self.recovery_epoch
                    .load(std::sync::atomic::Ordering::Relaxed)
                    == epoch
            }
        };
        if !current {
            self.requeue_restore(id).await?;
        }
        Ok(current)
    }

    /// Each tracked agent's execution, workspace and load. One waiting for
    /// its restore with every process proven stopped runs nothing; until that
    /// proof its load may still show.
    pub(super) async fn agent_executions(
        &self,
    ) -> std::collections::HashMap<String, (String, Activity)> {
        let agents = self.agents.lock().await;
        let waiting = self.restore_waiters.lock().unwrap();
        agents
            .iter()
            .map(|(id, agent)| {
                let activity = if waiting.contains_key(id) {
                    Activity::Stopped
                } else {
                    Activity::Running(agent.started.elapsed())
                };
                (id.clone(), (agent.workspace_id.clone(), activity))
            })
            .collect()
    }

    /// A scoped caller as a proposer, naming the agent whose execution it
    /// runs in; unscoped callers open directly.
    pub(crate) async fn proposer(
        &self,
        caller: Option<&crate::daemon::scope::Caller>,
    ) -> Option<crate::daemon::links::proposals::Proposer> {
        let caller = caller?;
        let agents = self.agents.lock().await;
        let agent = caller
            .execution_id()
            .and_then(|id| agents.get(id))
            .map(|agent| agent.name.clone());
        Some(crate::daemon::links::proposals::Proposer { agent })
    }

    /// The name of the connected agent that `execution` runs.
    pub(crate) async fn agent_name(&self, execution: &str) -> Option<String> {
        let agents = self.agents.lock().await;
        agents.get(execution).map(|agent| agent.name.clone())
    }

    /// Workspaces with a connected agent that is not being stopped.
    pub(crate) async fn running_agent_workspaces(&self) -> Vec<String> {
        let agents = self.agents.lock().await;
        let mut workspaces: Vec<_> = agents
            .values()
            .filter(|agent| !agent.stop.is_closed() && !*agent.stop.borrow())
            .map(|agent| agent.workspace_id.clone())
            .collect();
        workspaces.sort();
        workspaces.dedup();
        workspaces
    }

    pub(crate) async fn stop_agent_for_overload(&self, reason: &str) -> bool {
        !self
            .stop_agents(&Protection::load(reason), Select::One)
            .await
            .is_empty()
    }

    /// Stop every running agent, since any of them may be writing.
    pub(crate) async fn stop_agents_for_disk_space(
        &self,
        protection: &Protection,
    ) -> Vec<StoppedAgent> {
        self.stop_agents(protection, Select::All).await
    }

    async fn stop_agents(&self, protection: &Protection, select: Select) -> Vec<StoppedAgent> {
        // Without turns every agent counts as working, as one without hooks does.
        let turns = self.agent_turns().await.unwrap_or_else(|error| {
            log!("agent turns unavailable to protection: {error:#}");
            HashMap::new()
        });
        let stopped: Vec<_> = {
            let mut agents = self.agents.lock().await;
            let running = agents
                .iter()
                .filter(|(_, agent)| !agent.stop.is_closed() && !*agent.stop.borrow())
                .map(|(id, agent)| (id, agent.started));
            let selected: Vec<String> = match select {
                Select::One => first_to_stop(running, &turns)
                    .into_iter()
                    .cloned()
                    .collect(),
                Select::All => running.map(|(id, _)| id.clone()).collect(),
            };
            selected
                .into_iter()
                .filter_map(|id| {
                    let turn = turns.get(&id).map(|turn| turn.state);
                    self.protect(&id, agents.get_mut(&id)?, protection, turn)
                })
                .collect()
        };
        if !stopped.is_empty() {
            self.reset_overload_recovery();
        }
        for agent in &stopped {
            self.notify_protection_stop(agent).await;
        }
        stopped
    }

    /// Save the recovery handoff before delivering the stop, so a wrapper
    /// terminated under pressure can still be resumed.
    fn protect(
        &self,
        id: &str,
        agent: &mut Agent,
        protection: &Protection,
        turn: Option<AgentState>,
    ) -> Option<StoppedAgent> {
        let handoff = crate::execution::recovery::save_record(
            &self.paths,
            &agent.workspace_id,
            id,
            &agent.name,
            Some(&protection.reason),
            turn,
        );
        if let Err(error) = &handoff {
            log!("overload recovery handoff not saved: {error:#}");
        }
        let protection = restored_after(protection, turn);
        agent.overload = Some(protection.clone());
        // The monitor holds an operation, so no update can take the lock.
        agent.update_hold = self.background_operations.clone().try_read_owned().ok();
        agent.stop.send(true).ok()?;
        Some(StoppedAgent {
            id: id.into(),
            name: agent.name.clone(),
            workspace: agent.workspace.clone(),
            recover: agent.recover,
            handoff_saved: handoff.is_ok(),
            protection,
        })
    }

    async fn notify_protection_stop(&self, agent: &StoppedAgent) {
        let StoppedAgent {
            id,
            name,
            workspace,
            protection,
            ..
        } = agent;
        let recovery = if !agent.recover {
            "automatic recovery unavailable: configure [agent_resume] with a session restore command".into()
        } else if !self.config().overload.recovery.enabled {
            "automatic recovery disabled by overload.recovery.enabled".into()
        } else {
            format!(
                "automatic session restore waits until {}",
                protection.resumes_when
            )
        };
        self.notify(
            Some(workspace),
            NotificationKind::AgentStopped,
            format!("Stopping {name}: {}; {recovery}; after the wrapper exits, restore with shoal resume {workspace} --execution {id}{}; workspace and resource leases retained", protection.reason, if agent.handoff_saved { "" } else { "; recovery handoff could not be saved; run shoal stop if the wrapper exits" }),
        )
        .await;
    }
}

enum Select {
    /// The agent [`first_to_stop`] picks.
    One,
    All,
}

/// Of the `running` agents, by execution and start, the one whose stop costs
/// the least work: the one idle longest, otherwise the newest, which has the
/// least progress to lose.
fn first_to_stop<'a>(
    running: impl Iterator<Item = (&'a String, Instant)>,
    turns: &HashMap<String, Turn>,
) -> Option<&'a String> {
    let (idle, working): (Vec<_>, Vec<_>) =
        running.partition(|(id, _)| turns.get(*id).is_some_and(Turn::idle));
    match idle.into_iter().min_by_key(|(id, _)| turns[*id].since) {
        Some((id, _)) => Some(id),
        None => working
            .into_iter()
            .max_by_key(|(_, started)| *started)
            .map(|(id, _)| id),
    }
}

/// An agent asked to stop for protection.
pub(crate) struct StoppedAgent {
    id: String,
    pub name: String,
    pub workspace: String,
    recover: bool,
    handoff_saved: bool,
    /// What its automatic restore waits for.
    protection: Protection,
}

/// The `protection` an agent stopped in `turn` restores after: one stopped
/// between turns has nothing to do until an update arrives for it.
fn restored_after(protection: &Protection, turn: Option<AgentState>) -> Protection {
    match turn {
        Some(AgentState::Idle) => Protection {
            reason: protection.reason.clone(),
            resumes_when: "an update arrives for it".into(),
        },
        _ => protection.clone(),
    }
}

/// What a paused agent's process check concluded.
pub(crate) enum Pause {
    Paused,
    /// Processes survive, or the evidence cannot prove that none do.
    Unproven(String),
}

/// Scan for the execution's processes and combine the evidence.
async fn inspect_processes(
    record: &crate::model::Execution,
) -> anyhow::Result<crate::process::execution::Processes> {
    let ids = std::collections::HashSet::from([record.id.clone()]);
    let scan = crate::process::identity::scan(ids).await?;
    crate::process::execution::Processes::inspect(record, &scan).await
}

/// Probe until it finds nothing preventing proof or `patience` passes, and
/// return the last issue it reported.
async fn await_process_proof<F>(patience: Duration, mut probe: impl FnMut() -> F) -> Option<String>
where
    F: std::future::Future<Output = Option<String>>,
{
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        let issue = probe().await;
        if issue.is_none() || tokio::time::Instant::now() >= deadline {
            return issue;
        }
        tokio::time::sleep(timing::WORKSPACE_STOP_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod process_proof_tests;

#[cfg(test)]
mod tests;
