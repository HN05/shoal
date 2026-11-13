//! Connected agent metadata is deliberately transient: a daemon restart must
//! not turn an unknown execution into an automatic signal target.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::execution::recovery::LastSessionExt;
use rusqlite::OptionalExtension;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedRwLockReadGuard, watch};

use super::{
    GuardMode, Manager,
    agent_queue::{Grant, OneShotStop},
    agent_turns::Turn,
    capacity::Activity,
};
pub(crate) use crate::protocol::Protection;

use crate::daemon::log;
use crate::{
    agent::ModelChoice,
    daemon::notifications::NotificationKind,
    execution::TrackedAgent,
    protocol::timing,
    state::{AgentState, WorkspaceState},
};

mod attachment;
mod chat_questions;
mod forks;
mod idle_stop;
mod interrupts;
mod restart;
mod sleep_cost;
mod stops;
pub use restart::RestartWaits;
pub(crate) use restart::{Renewals, forget_ended_renewals};
pub(crate) use stops::recorded as stop_recorded;

/// Operations on the client type [`Protection`] that only the `shoal` command uses.
pub(crate) trait ProtectionExt {
    /// Memory or CPU pressure.
    fn load(reason: &str) -> Self;

    /// `shoal restart`, whose wrapper restores the agent once its processes
    /// stop. Wrappers handle it as any protection stop they restore.
    fn restart() -> Self;

    fn is_restart(&self) -> bool;

    /// An idle agent of a done workspace whose cleanup is held, which Shoal
    /// stops to free memory.
    fn idle_held() -> Self;

    /// An idle agent whose prompt cache expired, which Shoal stops to free
    /// memory under `[sleep] idle`.
    fn idle_expired() -> Self;

    /// An idle agent whose space sleeps after `shoal sleep`, which Shoal
    /// puts to sleep once that costs nothing.
    fn slept() -> Self;

    /// An idle stop for `reason`, whose restore waits for an update. Wrappers
    /// handle it as any protection stop they restore.
    fn idle(reason: &str) -> Self;

    /// An agent whose turn ended while its space is blocked, whose restore
    /// waits until nothing blocks the space.
    fn blocked() -> Self;

    /// Whether Shoal stopped the agent between turns, idle or blocked, not
    /// for load.
    fn is_idle(&self) -> bool;

    /// Whether Shoal stopped the agent for its blocked space.
    fn is_blocked(&self) -> bool;

    /// Whether `[overload.recovery]` decides its restore, as for a stop that
    /// load or disk space caused; idle stops restore whatever it says.
    fn follows_overload_recovery(&self) -> bool;

    /// Whether a done assignment cancels its restore. An idle stop's restore
    /// waits for an update all the same, since the agent ran until Shoal
    /// stopped it; a blocked one's would only restore an agent with nothing
    /// left to do, since `shoal done` removes the blockers.
    fn ends_with_assignment(&self) -> bool;
}

impl ProtectionExt for Protection {
    fn load(reason: &str) -> Self {
        Self {
            reason: reason.into(),
            resumes_when: crate::protocol::default_resumes_when(),
        }
    }

    fn restart() -> Self {
        Self {
            reason: RESTART_REASON.into(),
            resumes_when: "its processes stop".into(),
        }
    }

    fn is_restart(&self) -> bool {
        self.reason == RESTART_REASON
    }

    fn idle_held() -> Self {
        Self::idle(IDLE_HELD_REASON)
    }

    fn idle_expired() -> Self {
        Self::idle(IDLE_EXPIRED_REASON)
    }

    fn slept() -> Self {
        Self::idle(SLEPT_REASON)
    }

    fn idle(reason: &str) -> Self {
        Self {
            reason: reason.into(),
            resumes_when: WAKES_ON_UPDATE.into(),
        }
    }

    fn blocked() -> Self {
        Self {
            reason: BLOCKED_REASON.into(),
            resumes_when: WAKES_UNBLOCKED.into(),
        }
    }

    fn is_idle(&self) -> bool {
        [
            IDLE_HELD_REASON,
            IDLE_EXPIRED_REASON,
            SLEPT_REASON,
            BLOCKED_REASON,
        ]
        .contains(&self.reason.as_str())
    }

    fn is_blocked(&self) -> bool {
        self.reason == BLOCKED_REASON
    }

    fn follows_overload_recovery(&self) -> bool {
        !self.is_idle()
    }

    fn ends_with_assignment(&self) -> bool {
        !self.is_idle() || self.is_blocked()
    }
}

/// The stop reason a restarted agent's wrapper shows and saves.
const RESTART_REASON: &str = "restarted by shoal restart";

/// The stop reason of an idle agent of a done workspace whose cleanup is held.
const IDLE_HELD_REASON: &str = "idle in a done workspace whose cleanup is held";

/// The stop reason of an idle agent `[sleep] idle` puts to sleep.
const IDLE_EXPIRED_REASON: &str = "idle with its prompt cache expired";

/// The stop reason of an idle agent whose space sleeps after `shoal sleep`.
const SLEPT_REASON: &str = "shoal sleep";

/// The stop reason of an agent whose turn ended while its space is blocked,
/// which its recovery record keeps.
pub(crate) const BLOCKED_REASON: &str = "its space is blocked";

/// What wakes an agent stopped between turns, which has nothing to do until
/// then.
pub(crate) const WAKES_ON_UPDATE: &str = "an update arrives for it";

/// What wakes an agent stopped because its space is blocked.
pub(crate) const WAKES_UNBLOCKED: &str = "nothing blocks its space";

/// The agent a request comes from.
pub(crate) struct CallerAgent {
    pub(crate) execution: ExecutionId,
    pub(crate) name: crate::agent::Agent,
    /// The space its execution runs in.
    pub(crate) space_id: SpaceId,
}

pub(crate) struct Agent {
    name: crate::agent::Agent,
    /// The model and effort it runs with, which its recovery records keep.
    model: ModelChoice,
    /// The mode it started in, which its recovery records keep.
    mode: crate::config::modes::ModeName,
    /// The `--permission` narrowing it started with, which its recovery
    /// records keep.
    narrowing: crate::config::permissions::PermissionsConfig,
    workspace: SpaceName,
    workspace_id: SpaceId,
    /// Whether the user attends its space, so that Shoal never stops it to
    /// free capacity.
    attended: bool,
    started: Instant,
    recover: bool,
    stop: watch::Sender<bool>,
    /// Holds back a daemon update from the stop until the execution's
    /// connection takes over, so an update waits for the restore.
    update_hold: Option<OwnedRwLockReadGuard<()>>,
    /// The `shoal restart` of it in progress and those that ended.
    restarts: restart::Restarts,
    /// Whether a terminal shows the agent, as its wrapper reported last;
    /// `None` until it reports.
    attached: Option<bool>,
}

impl Agent {
    /// Its name with the model and effort it runs with.
    pub(super) fn tracked(&self) -> TrackedAgent {
        TrackedAgent {
            name: self.name.clone(),
            model: self.model.clone(),
            mode: self.mode.clone(),
            narrowing: self.narrowing,
        }
    }

    /// Cancel what the agent's connection keeps for its restore; the
    /// caller forgets its recorded stop.
    pub(super) fn cancel_recovery(&mut self) {
        self.update_hold = None;
        self.restarts.cancel();
    }

    /// Refuse another stop of the agent: protection `stopped` it and it
    /// waits for its restore, or a stop of it is in progress.
    fn ensure_not_stopped(&self, stopped: Option<&Protection>) -> anyhow::Result<()> {
        if let Some(protection) = stopped {
            anyhow::bail!(
                "{} is stopped ({}) and resumes once {}; restore it now with shoal queue start {}",
                self.name,
                protection.reason,
                protection.resumes_when,
                self.workspace_id
            );
        }
        anyhow::ensure!(
            !*self.stop.borrow() && self.restarts.current.is_none(),
            "{} is already stopping",
            self.name
        );
        Ok(())
    }

    /// Whether the agent is connected and nothing is stopping it.
    fn running(&self) -> bool {
        !self.stop.is_closed() && !*self.stop.borrow()
    }
}

impl Manager {
    /// Tell the user, or the coordinator, that the agent of `space` exited
    /// with `exit_code`, `complete` when its processes are proven stopped,
    /// or stopped for `overload`. An attended space's agent that exits
    /// cleanly tells no one: the user saw it end.
    pub(crate) async fn notify_agent_exit(
        &self,
        space: &crate::model::PresentSpace,
        agent: &crate::agent::Agent,
        exit_code: Option<i32>,
        complete: bool,
        overload: Option<(&ExecutionId, &str)>,
    ) {
        let clean = overload.is_none() && complete && exit_code.is_some();
        if clean && space.space_kind().attended() {
            return;
        }
        let message = if let Some((execution_id, reason)) = overload {
            let status = exit_code.map_or_else(
                || "without an exit code".into(),
                |code| format!("with code {code}"),
            );
            let recovery = if crate::execution::recovery::record_path(
                &self.paths,
                space.id(),
                execution_id,
            )
            .is_file()
            {
                format!(
                    "restore with shoal restore {} --execution {execution_id}",
                    space.number()
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
                "{agent} stopped after {reason} ({status}); {recovery}{reconcile}; {} and resource leases retained",
                space.kind()
            )
        } else {
            match exit_code {
                Some(code) if complete => format!("{agent} exited with code {code}"),
                Some(code) => format!(
                    "{agent} exited with code {code}, leaving processes behind; stop them with shoal stop {}",
                    space.number()
                ),
                None => format!(
                    "{agent} disconnected without reporting; stop what remains of it with shoal stop {}",
                    space.number()
                ),
            }
        };
        self.notify_space(
            space.name(),
            space.id(),
            NotificationKind::AgentExited,
            message,
        )
        .await;
    }

    /// Forget a finished agent's reported session, first keeping it as one of
    /// the last sessions of the space `space_id` when the agent `exited` on
    /// its own before the assignment finished, for `shoal restore` to continue.
    pub(crate) async fn end_agent_session(
        &self,
        space_id: &SpaceId,
        agent: &TrackedAgent,
        execution: &ExecutionId,
        exited: bool,
    ) {
        let kept = if exited {
            self.keep_agent_session(space_id, agent, execution).await
        } else {
            crate::execution::recovery::forget_session(&self.paths, space_id, execution)
        };
        if let Err(error) = kept {
            log!("agent session of {execution} not kept: {error:#}");
        }
    }

    /// Forget an ended agent's reported session, first keeping it as one of
    /// the last sessions of the space `space_id` while its assignment is
    /// unfinished.
    pub(crate) async fn keep_agent_session(
        &self,
        space_id: &SpaceId,
        agent: &TrackedAgent,
        execution: &ExecutionId,
    ) -> anyhow::Result<()> {
        use crate::execution::recovery::{self, LastSession};
        if self.completion(space_id).await?.is_none() {
            LastSession::save(&self.paths, space_id, agent, execution)?;
        }
        recovery::forget_session(&self.paths, space_id, execution)
    }

    /// A new agent or a finished assignment replaces the last sessions.
    pub(crate) fn forget_last_sessions(&self, workspace_id: &SpaceId) {
        let forgotten =
            crate::execution::recovery::LastSession::forget_all(&self.paths, workspace_id);
        if let Err(error) = forgotten {
            log!("last agent sessions of {workspace_id} not forgotten: {error:#}");
        }
    }

    /// What follows the exit of the agent of `space`: a workspace keeps a
    /// checkpoint of its worktree and runs its exit hook, then cleanup
    /// looks at the space again.
    pub(crate) async fn post_agent_exit(
        &self,
        space: &crate::model::PresentSpace,
        agent: &crate::agent::Agent,
        exit_code: Option<i32>,
        complete: bool,
    ) {
        if let Some(workspace) = space.worktree() {
            self.after_worktree_agent(workspace, agent, exit_code, complete)
                .await;
        }
        // A completion or forge transition may have happened while the agent
        // was running. Retry cleanup once the exit hook is finished.
        self.signals.wake_cleanup();
    }

    /// Keep a checkpoint of `workspace` once its agent's processes are
    /// proven `complete`, and run its exit hook, telling the user when it
    /// fails.
    async fn after_worktree_agent(
        &self,
        workspace: &crate::model::Workspace,
        agent: &crate::agent::Agent,
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
            self.notify_space(
                &workspace.name,
                &workspace.id,
                NotificationKind::HookFailed,
                format!("agent exited; {error:#}"),
            )
            .await;
        }
    }

    async fn run_agent_exit_hook(
        &self,
        workspace: &crate::model::Workspace,
        agent: &crate::agent::Agent,
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
            .gates
            .resource_guard(&workspace.id, GuardMode::Exclusive)?;
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

    async fn agent_exit_workspace_ready(&self, id: &SpaceId) -> anyhow::Result<bool> {
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

    /// Refuse an agent of its own in a workspace at a commit, which holds
    /// none; one that should work at a commit gets a branch workspace from
    /// it instead.
    pub(crate) fn ensure_holds_agents(workspace: &crate::model::Workspace) -> anyhow::Result<()> {
        anyhow::ensure!(
            workspace.branch.is_some(),
            "workspace {} is at a commit and holds no agent of its own; start the agent in a workspace on a branch, such as one split with --base <commit>",
            workspace.name
        );
        Ok(())
    }

    /// `running` is how long the agent has already run, so a reattached
    /// agent keeps its place in the newest-first overload order.
    pub(crate) async fn track_agent(
        &self,
        id: &ExecutionId,
        tracked: &TrackedAgent,
        workspace_id: &SpaceId,
        workspace: &SpaceName,
        recover: bool,
        running: Duration,
    ) {
        self.record_execution_agent(id, tracked).await;
        let attended = self.attended(workspace_id).await;
        let connections = self.executions.connections().await;
        let Some(stop) = connections.stop_channel(id) else {
            return;
        };
        let agent = Agent {
            name: tracked.name.clone(),
            model: tracked.model.clone(),
            mode: tracked.mode.clone(),
            narrowing: tracked.narrowing,
            workspace: workspace.clone(),
            workspace_id: workspace_id.clone(),
            attended,
            started: Instant::now()
                .checked_sub(running)
                .unwrap_or_else(Instant::now),
            recover,
            stop: stop.clone(),
            update_hold: None,
            restarts: Default::default(),
            attached: None,
        };
        connections
            .with_agents(|agents| agents.insert(id.clone(), agent))
            .await;
    }

    /// Whether the user attends the space `id`, as its kind says; a space
    /// that cannot be read counts as unattended.
    pub(super) async fn attended(&self, id: &SpaceId) -> bool {
        let id = id.to_owned();
        let kind = self
            .store
            .run(move |db| crate::daemon::spaces::space_kind(db, &id))
            .await;
        (kind.ok().flatten()).is_some_and(crate::model::SpaceKind::attended)
    }

    /// Keep the name, model, effort, mode and narrowing of the agent
    /// execution `id` runs with its record, which outlives this daemon,
    /// unlike the tracked agent; the mode and narrowing become its
    /// space's.
    async fn record_execution_agent(&self, id: &ExecutionId, agent: &TrackedAgent) {
        let (execution, agent) = (id.to_owned(), agent.clone());
        let recorded = self
            .store
            .run(move |db| {
                let narrowing = crate::daemon::store::narrowing_column(&agent.narrowing)?;
                db.execute(
                    "UPDATE executions SET agent=?2,model=?3,effort=?4,mode=?5,narrowing=?6
                     WHERE id=?1",
                    rusqlite::params![
                        execution,
                        agent.name,
                        agent.model.model,
                        agent.model.effort,
                        agent.mode,
                        narrowing
                    ],
                )?;
                for table in ["workspaces", "chats"] {
                    db.execute(
                        &format!(
                            "UPDATE {table} SET mode=?2,narrowing=?3
                             WHERE id=(SELECT space_id FROM executions WHERE id=?1)"
                        ),
                        rusqlite::params![execution, agent.mode, narrowing],
                    )?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = recorded {
            log!("agent of execution {id} not recorded: {error:#}");
        }
    }

    /// Prove that a protection-stopped agent ended, then forget its stopped
    /// child. Identity-verified processes it left outside its process group,
    /// such as background shell commands, are stopped as `shoal stop`
    /// stops them; the waiting wrapper is not a target. Reports what survives
    /// when `patience` passes without proof.
    pub(crate) async fn pause_agent_execution(
        &self,
        id: &ExecutionId,
        workspace_id: &SpaceId,
        patience: Duration,
    ) -> anyhow::Result<Pause> {
        let record = self
            .workspace_executions(workspace_id)
            .await?
            .into_iter()
            .find(|execution| execution.id == *id)
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

    /// Stop what a command or agent that reported its exit left running
    /// outside its process group, such as background shell commands, so
    /// nothing outlives it. An agent's turn is kept first, while that work
    /// still shows whether it was in flight. The completion scan that follows
    /// decides whether anything survived.
    pub(crate) async fn stop_exited_processes(&self, id: &ExecutionId) {
        let query_id = id.to_owned();
        let stopped = async {
            let record = self
                .store
                .run(move |db| crate::daemon::store::find_execution(db, &query_id))
                .await?
                .ok_or_else(|| anyhow::anyhow!("execution no longer exists"))?;
            let scan = scan_processes(&record).await?;
            self.keep_stopped_turn(id, Some(&scan)).await;
            self.inspect_execution(&record, &scan)
                .await?
                .stop_owned()
                .await
        };
        if let Err(error) = stopped.await {
            log!("processes of exited execution {id} not stopped: {error:#}");
        }
    }

    /// Stop the execution's verified survivors and report what keeps its
    /// processes from being proven stopped, if anything does.
    async fn stop_agent_survivors(&self, record: &crate::model::Execution) -> Option<String> {
        let processes = match self.inspect_processes(record).await {
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

    /// Scan for the execution's processes and combine the evidence.
    async fn inspect_processes(
        &self,
        record: &crate::model::Execution,
    ) -> anyhow::Result<crate::process::execution::Processes> {
        let scan = scan_processes(record).await?;
        self.inspect_execution(record, &scan).await
    }

    /// Tell the user why a protection-stopped agent's automatic restore waits
    /// beyond healthy readings, and a waiting `shoal restart` of it.
    pub(crate) async fn notify_restore_waits(&self, id: &ExecutionId, issue: &str) {
        let Some((name, workspace, workspace_id)) = self
            .executions
            .with_agent(id, |agent| {
                let (workspace, id) = (agent.workspace.clone(), agent.workspace_id.clone());
                (agent.name.clone(), workspace, id)
            })
            .await
        else {
            return;
        };
        log!("restore of {name} in {workspace} waits for its processes to stop: {issue}");
        self.notify_space(
            &workspace,
            &workspace_id,
            NotificationKind::RecoveryBlocked,
            format!("Automatic restore of {name} waits until its remaining processes exit: {issue}; shoal stop {workspace_id} cancels the restore"),
        )
        .await;
        self.restart_waits(id, issue).await;
    }

    /// Resume the stopped agent `id`, which `grant` admitted when its
    /// automatic restore did, unless the system is shutting down: under the
    /// queue gate, so a shutdown that begins meanwhile finds it running.
    pub(crate) async fn resume_agent_execution(
        &self,
        id: &ExecutionId,
        grant: Option<Grant>,
    ) -> anyhow::Result<bool> {
        let _gate = self.agent_queue.lock_admission().await;
        anyhow::ensure!(!self.agent_queue.suspended(), "the system is shutting down");
        // A stop of the execution, which cancels its restore, takes this lock
        // too, so the restore it finds stays decided.
        let connections = self.executions.connections().await;
        // The agent's turns count again before it runs, and it reports its
        // own.
        let protected = self.agent_overload(id).await.is_some();
        if protected || self.restart_requested(id).await {
            self.forget_resumed_stop(id).await?;
        }
        let stop = connections
            .stop_channel(id)
            .ok_or_else(|| anyhow::anyhow!("execution disconnected"))?;
        let resumed = connections
            .with_agents(|agents| {
                let agent = agents
                    .get_mut(id)
                    .ok_or_else(|| anyhow::anyhow!("agent no longer exists"))?;
                let restarting = agent
                    .restarts
                    .current
                    .as_mut()
                    .map(restart::Restart::resume)
                    .is_some();
                if !protected && !restarting {
                    return Ok(None);
                }
                stop.send(false)?;
                agent.update_hold = None;
                agent.started = Instant::now();
                anyhow::Ok(Some((
                    agent.name.clone(),
                    (agent.workspace.clone(), agent.workspace_id.clone()),
                    restarting,
                )))
            })
            .await?;
        drop(connections);
        let Some((name, (workspace, workspace_id), restarting)) = resumed else {
            return Ok(false);
        };
        // `shoal restart` reports its own restarts.
        if restarting {
            return Ok(true);
        }
        let cause = match grant {
            Some(Grant::Update) => "for an update",
            Some(Grant::Queue) => "from the agent queue",
            _ => "after sustained healthy readings",
        };
        self.notify_space(
            &workspace,
            &workspace_id,
            NotificationKind::AgentResumed,
            format!("Restoring {name} {cause}"),
        )
        .await;
        Ok(true)
    }

    /// The guard a protection stop took for the execution's connection.
    pub(crate) async fn take_update_hold(
        &self,
        id: &ExecutionId,
    ) -> Option<OwnedRwLockReadGuard<()>> {
        self.executions
            .with_agent(id, |agent| agent.update_hold.take())
            .await
            .flatten()
    }

    /// Queue the automatic restore of the protection-stopped agent `id`.
    pub(crate) async fn queue_agent_restore(&self, id: &ExecutionId) -> anyhow::Result<()> {
        let (name, workspace_id) = self
            .executions
            .with_agent(id, |agent| (agent.name.clone(), agent.workspace_id.clone()))
            .await
            .ok_or_else(|| anyhow::anyhow!("agent no longer exists"))?;
        self.queue_restore(id, &name, &workspace_id).await
    }

    /// Wait until the agent queue admits the restore of `id` and the
    /// admission holds through the checks of its workspace.
    pub(crate) async fn admit_agent_restore(
        &self,
        id: &ExecutionId,
        workspace_id: &SpaceId,
    ) -> anyhow::Result<Grant> {
        loop {
            let grant = self.await_restore_admission(id).await?;
            if self.confirm_restore(id, workspace_id, grant).await? {
                return Ok(grant);
            }
        }
    }

    /// Check that the space `id` can take an agent's restore: a workspace
    /// is ready and still the recorded worktree, so a replaced one never
    /// receives the agent, and a chat's directory is there.
    pub(crate) async fn verify_restore_space(&self, id: &SpaceId) -> anyhow::Result<()> {
        match self.space(&SpaceSelector::id(id)).await? {
            crate::model::PresentSpace::Workspace(workspace) => {
                self.verify_worktree(&workspace).await?;
                anyhow::ensure!(
                    workspace.state == crate::state::WorkspaceState::Ready,
                    "workspace is no longer ready for agent recovery"
                );
                Ok(())
            }
            crate::model::PresentSpace::Chat(chat) => self.chat_path(&chat).await.map(drop),
        }
    }

    /// Check that an admitted restore's workspace is ready and verified. A
    /// stop or reload since healthy readings admitted it revokes it, since
    /// they no longer hold, and returns it to its place; an update's admission
    /// holds.
    pub(super) async fn confirm_restore(
        &self,
        id: &ExecutionId,
        workspace_id: &SpaceId,
        grant: Grant,
    ) -> anyhow::Result<bool> {
        self.verify_restore_space(workspace_id).await?;
        let current = match grant {
            Grant::Update | Grant::Queue => true,
            Grant::Recovery(epoch) => self.overload_recovery.epoch() == epoch,
        };
        if !current {
            self.requeue_restore(id).await?;
        }
        Ok(current)
    }

    /// Each tracked agent's execution, workspace and load, with the memory
    /// its workspace used at the latest usage sample. One waiting for its
    /// restore with every process proven stopped runs nothing; until that
    /// proof its load may still show.
    pub(super) async fn agent_executions(
        &self,
    ) -> std::collections::HashMap<ExecutionId, (SpaceId, Activity)> {
        let waiting = self.agent_queue.restore_waiters();
        let agents: Vec<(ExecutionId, SpaceId, std::time::Duration)> = self
            .executions
            .with_agents(|agents| {
                agents
                    .iter()
                    .map(|(id, agent)| {
                        (
                            id.clone(),
                            agent.workspace_id.clone(),
                            agent.started.elapsed(),
                        )
                    })
                    .collect()
            })
            .await;
        agents
            .into_iter()
            .map(|(id, workspace, age)| {
                let activity = if waiting.contains(&id) {
                    Activity::Stopped
                } else {
                    Activity::Running {
                        age,
                        memory: self
                            .workspace_usage(&workspace)
                            .map(|usage| usage.memory_bytes),
                    }
                };
                (id, (workspace, activity))
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
        let agent = match caller.execution_id() {
            Some(id) => self.agent_name(id).await,
            None => None,
        };
        Some(crate::daemon::links::proposals::Proposer { agent })
    }

    /// The agent a request from `caller` comes from: the one its execution
    /// runs, else the one whose process tree registered it.
    pub(crate) async fn caller_agent(
        &self,
        caller: &crate::daemon::scope::Caller,
    ) -> Option<CallerAgent> {
        let execution = caller.execution.as_ref()?;
        if let Some(own) = self.execution_agent(&execution.id).await {
            return Some(own);
        }
        self.execution_agent(execution.agent_execution.as_ref()?)
            .await
    }

    /// The agent execution `id` runs: tracked while connected, else as its
    /// record names it, as when a command it ran reattaches first after a
    /// restart.
    async fn execution_agent(&self, id: &ExecutionId) -> Option<CallerAgent> {
        let tracked = self
            .executions
            .with_agent(id, |agent| (agent.name.clone(), agent.workspace_id.clone()))
            .await;
        let (name, space_id) = match tracked {
            Some(tracked) => tracked,
            None => self.recorded_agent(id).await?,
        };
        Some(CallerAgent {
            execution: id.to_owned(),
            // Named as `--agent` and recovery records name it.
            name: name.normalized(),
            space_id,
        })
    }

    /// The agent and space the record of execution `id` names, if it ran
    /// one.
    async fn recorded_agent(&self, id: &ExecutionId) -> Option<(crate::agent::Agent, SpaceId)> {
        let execution = id.to_owned();
        let recorded = self
            .store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT agent,space_id FROM executions WHERE id=?1 AND agent IS NOT NULL",
                        [execution],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?)
            })
            .await;
        recorded.unwrap_or_else(|error| {
            log!("agent of execution {id} unreadable: {error:#}");
            None
        })
    }

    /// The name of the connected agent that `execution` runs.
    pub(crate) async fn agent_name(&self, execution: &ExecutionId) -> Option<crate::agent::Agent> {
        self.executions
            .with_agent(execution, |agent| agent.name.clone())
            .await
    }

    /// Workspaces with a connected agent that is not being stopped.
    pub(crate) async fn running_agent_workspaces(&self) -> Vec<SpaceId> {
        let mut workspaces: Vec<_> = self
            .executions
            .with_agents(|agents| {
                agents
                    .values()
                    .filter(|agent| agent.running())
                    .map(|agent| agent.workspace_id.clone())
                    .collect()
            })
            .await;
        workspaces.sort();
        workspaces.dedup();
        workspaces
    }

    /// Spaces with a recorded agent execution, connected or not, such as
    /// one whose wrapper waits to reattach after a daemon restart.
    pub(crate) async fn recorded_agent_spaces(&self) -> anyhow::Result<HashSet<SpaceId>> {
        let agents = self
            .store
            .run(crate::daemon::store::agent_executions)
            .await?;
        Ok(agents.into_keys().collect())
    }

    /// Stop one agent under `reason`: the newest one-shot run first, which
    /// has no session to lose and starts over once readings are healthy.
    pub(crate) async fn stop_agent_for_overload(&self, reason: &str) -> bool {
        if self.agent_queue.stop_oneshots(OneShotStop::Newest) > 0 {
            log!("{reason}: stopped a one-shot agent run");
            return true;
        }
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
        let turns = self.protection_turns().await;
        self.stop_agents_by_turn(protection, select, turns).await
    }

    /// The agents' turns as protection reads them; without them every agent
    /// counts as working, as one without hooks does.
    async fn protection_turns(&self) -> HashMap<ExecutionId, Turn> {
        self.agent_turns().await.unwrap_or_else(|error| {
            log!("agent turns unavailable to protection: {error:#}");
            HashMap::new()
        })
    }

    /// Stop the agents `select` picks by `turns`. One picked idle stops only
    /// while no turn started since `turns` were read, so `select` picks
    /// again by the turns read then, a few times at most.
    async fn stop_agents_by_turn(
        &self,
        protection: &Protection,
        select: Select,
        mut turns: HashMap<ExecutionId, Turn>,
    ) -> Vec<StoppedAgent> {
        let mut stopped = Vec::new();
        for _ in 0..PROTECTION_CHOICES {
            let mut raced = false;
            for (id, turn) in self.choose_stops(select, &turns).await {
                match self.protect(&id, protection, turn, None).await {
                    Some(agent) => stopped.push(agent),
                    None => raced |= turn.is_some_and(|turn| turn.idle()),
                }
            }
            if !raced {
                break;
            }
            turns = self.protection_turns().await;
        }
        if !stopped.is_empty() {
            self.overload_recovery.reset();
        }
        for agent in &stopped {
            self.notify_protection_stop(agent).await;
        }
        stopped
    }

    /// The running agents `select` picks by `turns`, each with its turn.
    async fn choose_stops(
        &self,
        select: Select,
        turns: &HashMap<ExecutionId, Turn>,
    ) -> Vec<(ExecutionId, Option<Turn>)> {
        // Without the modes every agent counts as heavy, as before weights.
        let light = self.light().await.unwrap_or_else(|error| {
            log!("agent weights unavailable to protection: {error:#}");
            Default::default()
        });
        self.executions
            .with_agents(|agents| {
                let running =
                    agents
                        .iter()
                        .filter(|(_, agent)| agent.running())
                        .map(|(id, agent)| Running {
                            id,
                            started: agent.started,
                            attached: agent.attached == Some(true),
                            attended: agent.attended,
                        });
                let selected: Vec<ExecutionId> = match select {
                    // Load stops the heavy agents the user does not attend.
                    Select::One => {
                        let heavy =
                            running.filter(|agent| !agent.attended && !light.execution(agent.id));
                        first_to_stop(heavy, turns).into_iter().cloned().collect()
                    }
                    Select::All => running.map(|agent| agent.id.clone()).collect(),
                };
                selected
                    .into_iter()
                    .map(|id| {
                        let turn = turns.get(&id).copied();
                        (id, turn)
                    })
                    .collect()
            })
            .await
    }

    /// Stop the agent of `id`, chosen in `turn`, for `protection`: save the
    /// recovery handoff, then record the stop and deliver it. A stop recorded or
    /// delivered meanwhile keeps the agent from this one, and so does a turn
    /// that started since one chosen idle, or for a stop that frees memory
    /// with a cache `cache_expired_at` must find expired, a turn end that
    /// renewed it.
    async fn protect(
        &self,
        id: &ExecutionId,
        protection: &Protection,
        chosen: Option<Turn>,
        cache_expired_at: Option<crate::time::UnixTime>,
    ) -> Option<StoppedAgent> {
        let turn = chosen.map(|turn| turn.state);
        let between = chosen.filter(Turn::idle).map(|turn| stops::Between {
            turn: turn.generation,
            cache_expired_at,
        });
        let mut stopped = self.stop_target(id, protection, turn).await?;
        // Nothing presses an idle stop, so it waits for a record that
        // restores the agent.
        let handoff = stops::Handoff {
            reason: &protection.reason,
            turn,
            required: protection.is_idle(),
        };
        match (self.decide_stop(id, &stopped.protection, between, handoff)).await {
            Ok(saved) => {
                stopped.handoff_saved = saved;
                Some(stopped)
            }
            // Load and disk space press, so a store that cannot record the
            // stop, as on a full disk, does not keep the agent running.
            Err(error) if error.is::<stops::Unrecorded>() && !protection.is_idle() => {
                self.stop_unrecorded(stopped, &error).await
            }
            Err(error) => {
                let (name, reason) = (&stopped.name, &protection.reason);
                log!("{name} not stopped for {reason}: {error:#}");
                None
            }
        }
    }

    /// Stop the `stopped` agent, whose stop the store could not record, as
    /// `shoal stop` does, saving its session for `shoal restore` instead of
    /// an automatic restore.
    async fn stop_unrecorded(
        &self,
        mut stopped: StoppedAgent,
        error: &anyhow::Error,
    ) -> Option<StoppedAgent> {
        let (name, reason) = (&stopped.name, &stopped.protection.reason);
        log!("{name} stops for {reason} without automatic restore: {error:#}");
        let request = super::StopRequest::unrecorded(reason);
        let connections = self.executions.connections().await;
        match connections.stop(&stopped.id, &request).await {
            Ok(true) => {
                stopped.recorded = false;
                Some(stopped)
            }
            Ok(false) => None,
            Err(error) => {
                log!("{name} not stopped for {reason}: {error:#}");
                None
            }
        }
    }

    /// How a stop for `protection` in `turn` stops the tracked agent of `id`.
    async fn stop_target(
        &self,
        id: &ExecutionId,
        protection: &Protection,
        turn: Option<AgentState>,
    ) -> Option<StoppedAgent> {
        (self.executions)
            .with_agent(id, |agent| StoppedAgent {
                id: id.clone(),
                name: agent.name.clone(),
                workspace: agent.workspace.clone(),
                workspace_id: agent.workspace_id.clone(),
                recover: agent.recover,
                recorded: true,
                handoff_saved: false,
                protection: restored_after(protection, turn),
            })
            .await
    }

    /// Record that `protection` stops the agent of `id`, chosen `between`
    /// turns when given, and deliver the stop with its `handoff`, forgetting
    /// the record when it cannot be delivered. Only the stop recorded saves
    /// a handoff, so one that loses leaves the winner's. Whether the handoff
    /// was saved.
    async fn decide_stop(
        &self,
        id: &ExecutionId,
        protection: &Protection,
        between: Option<stops::Between>,
        handoff: stops::Handoff<'_>,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            self.record_agent_stop(id, protection, between).await?,
            "it is stopped already or no longer idle in the turn it was chosen in"
        );
        let delivered = self.deliver_stop(id, handoff).await;
        if delivered.is_err() {
            self.forget_agent_stop(id).await;
        }
        delivered
    }

    /// Whether `shoal done` finished the workspace's assignment, which leaves
    /// an agent `protection` stopped nothing to do, so its restore waits for
    /// `shoal restore`.
    pub(crate) async fn done_cancels_restore(
        &self,
        workspace_id: &SpaceId,
        protection: &Protection,
    ) -> anyhow::Result<bool> {
        Ok(protection.ends_with_assignment() && self.completion(workspace_id).await?.is_some())
    }

    async fn notify_protection_stop(&self, agent: &StoppedAgent) {
        let StoppedAgent {
            id,
            name,
            workspace,
            workspace_id,
            protection,
            ..
        } = agent;
        let done =
            (self.done_cancels_restore(workspace_id, protection).await).unwrap_or_else(|error| {
                log!("completion unavailable to the stop notice: {error:#}");
                false
            });
        let recovery = if !agent.recorded {
            "its stop could not be recorded, so it has no automatic restore".into()
        } else if !agent.recover {
            "automatic recovery unavailable: configure [agents.<name>] resume_command with a session restore command".into()
        } else if protection.follows_overload_recovery()
            && !self.config().machine.overload.recovery.enabled
        {
            "automatic recovery disabled by machine.overload.recovery.enabled".into()
        } else if done {
            "no automatic restore, since the workspace is marked done; shoal restore withdraws the completion".into()
        } else {
            format!(
                "automatic session restore waits until {}",
                protection.resumes_when
            )
        };
        self.notify_space(
            workspace,
            workspace_id,
            NotificationKind::AgentStopped,
            format!("Stopping {name}: {}; {recovery}; after the wrapper exits, restore with shoal restore {workspace_id} --execution {id}{}; workspace and resource leases retained", protection.reason, if agent.handoff_saved || !agent.recorded { "" } else { "; recovery handoff could not be saved; run shoal stop if the wrapper exits" }),
        )
        .await;
    }
}

/// How many times protection picks agents to stop again after a turn
/// started in one it picked idle.
const PROTECTION_CHOICES: usize = 3;

#[derive(Clone, Copy)]
enum Select {
    /// The heavy agent [`first_to_stop`] picks, for load.
    One,
    All,
}

/// A running agent protection may stop.
struct Running<'a> {
    id: &'a ExecutionId,
    started: Instant,
    /// Whether a terminal shows it, so the user may be working with it.
    attached: bool,
    /// Whether the user attends its space.
    attended: bool,
}

/// Of the `running` agents, one nobody is attached to while there is one,
/// whose stop costs the least work.
fn first_to_stop<'a>(
    running: impl Iterator<Item = Running<'a>>,
    turns: &HashMap<ExecutionId, Turn>,
) -> Option<&'a ExecutionId> {
    let (attached, unattached): (Vec<_>, Vec<_>) = running.partition(|agent| agent.attached);
    least_work(unattached, turns).or_else(|| least_work(attached, turns))
}

/// Of `agents`, the one idle longest, otherwise the newest, which has the
/// least progress to lose.
fn least_work<'a>(
    agents: Vec<Running<'a>>,
    turns: &HashMap<ExecutionId, Turn>,
) -> Option<&'a ExecutionId> {
    let (idle, working): (Vec<_>, Vec<_>) = agents
        .into_iter()
        .partition(|agent| turns.get(agent.id).is_some_and(Turn::idle));
    match idle.into_iter().min_by_key(|agent| turns[agent.id].since) {
        Some(agent) => Some(agent.id),
        None => working
            .into_iter()
            .max_by_key(|agent| agent.started)
            .map(|agent| agent.id),
    }
}

/// An agent asked to stop for protection.
pub(crate) struct StoppedAgent {
    id: ExecutionId,
    pub name: crate::agent::Agent,
    pub workspace: SpaceName,
    workspace_id: SpaceId,
    recover: bool,
    /// Whether the store recorded its stop, without which it has no
    /// automatic restore.
    recorded: bool,
    handoff_saved: bool,
    /// What its automatic restore waits for.
    protection: Protection,
}

/// The `protection` an agent stopped in `turn` restores after: one stopped
/// between turns has nothing to do until an update arrives for it, unless
/// it stopped because its space is blocked.
fn restored_after(protection: &Protection, turn: Option<AgentState>) -> Protection {
    match turn {
        Some(AgentState::Idle) if !protection.is_blocked() => Protection {
            reason: protection.reason.clone(),
            resumes_when: WAKES_ON_UPDATE.into(),
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

/// Scan for the execution's processes.
async fn scan_processes(
    record: &crate::model::Execution,
) -> anyhow::Result<crate::process::identity::Scan> {
    let ids = std::collections::HashSet::from([record.id.clone()]);
    crate::process::identity::scan(ids).await
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
