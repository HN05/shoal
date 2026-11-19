//! A stopped wrapper waits until its processes are proven stopped and the
//! agent queue admits its restore, or only for the proof when `shoal restart`
//! stopped it, and finishes normally when recovery is cancelled or a restart
//! leaves the agent to a new wrapper. Failed ownership checks never authorize
//! a replacement process.
use crate::daemon::agents::ProtectionExt;
use anyhow::{Context, Result, ensure};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::time::Duration;
use tokio::{
    net::unix::OwnedWriteHalf,
    sync::{OwnedRwLockReadGuard, mpsc, watch},
};

use super::{
    agent_queue::{Grant, RestoreLink},
    agents::{Pause, Protection},
    executions::StopRequest,
    manager::Manager,
};
use crate::daemon::log;
use crate::{
    config::permissions::PermissionsConfig,
    execution::recovery,
    model::PortReservation,
    protocol::{self, Control, ExecutionEvent, timing},
};
use shoal_protocol::space_names::SpaceSelector;

pub(super) enum Action {
    Resume(Resumed),
    Stop(Cancel),
    Finished(i32),
}

/// A resume of the waiting agent: the workspace's ports, the narrowing its
/// saved record gives it in place of the one it ran with, from a permission
/// change made while it waited, and the claim on that record, which keeps
/// such changes out until the restored agent starts.
pub(super) struct Resumed {
    pub ports: Vec<PortReservation>,
    pub narrowing: Option<PermissionsConfig>,
    pub claim: Option<recovery::RecoveryClaim>,
}

/// A cancelled restore, as the wrapper reports it.
pub(super) struct Cancel {
    /// The reason the wrapper shows, if there is one.
    pub reason: Option<String>,
    /// Removal cancelled it, and the removal record keeps the agent.
    pub removed: bool,
}

impl Cancel {
    fn because(reason: impl Into<String>) -> Self {
        Self {
            reason: Some(reason.into()),
            removed: false,
        }
    }

    /// The cancellation of a stop request, such as `shoal stop` or removal.
    fn requested(request: Option<StopRequest>) -> Self {
        Self {
            removed: request
                .as_ref()
                .is_some_and(StopRequest::keeps_removed_agents),
            reason: request.map(|request| request.reason),
        }
    }

    /// The control that tells the waiting wrapper.
    pub(super) fn control(self) -> Control {
        Control::Stop {
            reason: self.reason,
            removed: self.removed,
        }
    }
}

/// The wrapper of a paused execution: what reaches it with what holds its
/// restore back, when its release reads that, the narrowing its agent runs
/// under, and whether it restores the agent under another.
pub(super) struct Wrapper<'a> {
    pub notices: Option<&'a mut OwnedWriteHalf>,
    pub runs_under: Option<PermissionsConfig>,
    pub takes_narrowing: bool,
}

/// `hold` is the operation guard that holds back a daemon update while the
/// wrapper waits, which a wait for an update lets go of.
pub(super) async fn pause(
    manager: &Manager,
    execution_id: &ExecutionId,
    workspace_id: &SpaceId,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    stop: &mut watch::Receiver<bool>,
    wrapper: Wrapper<'_>,
    hold: &mut Option<OwnedRwLockReadGuard<()>>,
) -> Result<Action> {
    let Wrapper {
        notices,
        runs_under,
        takes_narrowing,
    } = wrapper;
    let resumes = Resumes {
        manager,
        execution_id,
        workspace_id,
        runs_under,
        takes_narrowing,
    };
    if manager.restart_requested(execution_id).await {
        return restart(resumes, incoming, stop, notices).await;
    }
    let Some(protection) = manager.agent_overload(execution_id).await else {
        return Ok(Action::Stop(requested_cancel(manager, execution_id).await));
    };
    let follows_policy = protection.follows_overload_recovery();
    // Stop survivors before anything can cancel the restore, so every outcome
    // leaves only processes Shoal cannot stop. The wrapper finishes on each,
    // and the daemon's completion scan decides whether they leave it unknown.
    let stopped = match manager
        .pause_agent_execution(execution_id, workspace_id, timing::PROCESS_PROOF_TIMEOUT)
        .await
    {
        Ok(stopped) => stopped,
        Err(error) => return Ok(restore_failed(error)),
    };
    match manager
        .done_cancels_restore(workspace_id, &protection)
        .await
    {
        Ok(false) => {}
        Ok(true) => return Ok(cancelled_as_done()),
        Err(error) => return Ok(restore_failed(error)),
    }
    // Take a place in the agent queue as the agent stops, so restores and
    // launches start in the order they arrived.
    if let Err(error) = manager.queue_agent_restore(execution_id).await {
        return Ok(restore_failed(error));
    }
    let ready = tokio::select! {
        biased;
        action = interrupted(manager, execution_id, incoming, stop) => return action,
        () = recovery_disabled(manager), if follows_policy => {
            return Ok(Action::Stop(Cancel::because("automatic recovery was disabled")));
        }
        ready = async {
            await_stopped(manager, execution_id, workspace_id, stopped, notices).await?;
            // Every process the agent left has stopped writing.
            manager.checkpoint_after_agent(workspace_id).await;
            let link = manager.agent_queue.link_restore(execution_id);
            admit_restore(manager, &link, workspace_id, hold).await
        } => ready,
    };
    let resume = async {
        let grant = ready?;
        ensure!(
            !follows_policy || manager.config().machine.overload.recovery.enabled,
            "overload recovery was disabled while the agent waited"
        );
        prepare_resume(resumes, grant, &protection).await
    }
    .await;
    Ok(resume.unwrap_or_else(restore_failed))
}

/// Restore an agent `shoal restart` stopped once its processes are proven
/// stopped, here or in a new wrapper, answering the restart when it does
/// neither.
async fn restart(
    resumes: Resumes<'_>,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    stop: &mut watch::Receiver<bool>,
    notices: Option<&mut OwnedWriteHalf>,
) -> Result<Action> {
    let (manager, execution_id) = (resumes.manager, resumes.execution_id);
    let action = restart_once_stopped(resumes, incoming, stop, notices).await;
    if let Ok(Action::Stop(cancel)) = &action {
        let reason = cancel
            .reason
            .clone()
            .unwrap_or_else(|| "the restart was cancelled".into());
        manager.fail_restart(execution_id, reason).await;
    }
    action
}

async fn restart_once_stopped(
    resumes: Resumes<'_>,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    stop: &mut watch::Receiver<bool>,
    notices: Option<&mut OwnedWriteHalf>,
) -> Result<Action> {
    let (manager, execution_id, workspace_id) =
        (resumes.manager, resumes.execution_id, resumes.workspace_id);
    let stopped = match manager
        .pause_agent_execution(execution_id, workspace_id, timing::PROCESS_PROOF_TIMEOUT)
        .await
    {
        Ok(stopped) => stopped,
        Err(error) => return Ok(restore_failed(error)),
    };
    let ready = tokio::select! {
        biased;
        action = interrupted(manager, execution_id, incoming, stop) => return action,
        ready = async {
            await_stopped(manager, execution_id, workspace_id, stopped, notices).await?;
            manager.checkpoint_after_agent(workspace_id).await;
            anyhow::Ok(())
        } => ready,
    };
    let resume = async {
        ready?;
        manager.verify_restore_space(workspace_id).await?;
        if manager.settle_restart(execution_id).await {
            manager.give_restart_change(execution_id).await?;
            resumes.resume(None).await
        } else {
            hand_over(manager, execution_id).await
        }
    }
    .await;
    Ok(resume.unwrap_or_else(restore_failed))
}

/// Finish the wrapper of a restarted agent, keeping its record, which a new
/// wrapper restores once this one finished. A wrapper of any release
/// finishes on the stop that cancels its restore.
async fn hand_over(manager: &Manager, execution_id: &ExecutionId) -> Result<Action> {
    manager.hand_over_restart(execution_id).await?;
    Ok(Action::Stop(Cancel::because(
        "the agent restarts in a new terminal",
    )))
}

/// Resolves when the wrapper finishes or a stop cancels the restore it waits
/// for, starting that restore when the user wakes the agent meanwhile: at
/// once, or once the agent's processes have stopped.
async fn interrupted(
    manager: &Manager,
    execution_id: &ExecutionId,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    stop: &mut watch::Receiver<bool>,
) -> Result<Action> {
    let mut woken = false;
    loop {
        tokio::select! {
            biased;
            event = incoming.recv() => match event.context("execution disconnected")?? {
                ExecutionEvent::Finished { exit_code } => return Ok(Action::Finished(exit_code)),
                ExecutionEvent::Wake => {
                    // A key on the agent's sleep screen is the user's.
                    manager.record_agent_interaction(execution_id).await;
                    woken = !wake_restore(manager, execution_id).await;
                }
                _ => return Err(anyhow::anyhow!("unexpected event while the agent waits for its restore")),
            },
            changed = stop.changed() => {
                changed?;
                return Ok(Action::Stop(requested_cancel(manager, execution_id).await));
            }
            () = tokio::time::sleep(WAKE_RETRY_INTERVAL), if woken => {
                woken = !wake_restore(manager, execution_id).await;
            }
        }
    }
}

/// How soon a wake that came while the agent still stopped tries again.
const WAKE_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// Start the waiting restore of the agent `execution_id` ran, which the user
/// woke in its place; its admission then resumes the wrapper. Whether the
/// wake is settled: `false` while the agent still stops, which leaves it to
/// try again, and a failure is logged.
async fn wake_restore(manager: &Manager, execution_id: &ExecutionId) -> bool {
    match manager.wake_waiting_restore(execution_id).await {
        Ok(started) => started,
        Err(error) => {
            log!("agent restore not woken: {error:#}");
            true
        }
    }
}

/// Wait until the agent queue hands the restore to this wrapper over its
/// `link`. A wait for an update may last indefinitely, so it lets a daemon
/// update through, which hands the wait to the next daemon as other
/// shutdowns do, and takes `hold` back before the agent resumes.
async fn admit_restore(
    manager: &Manager,
    link: &RestoreLink<'_>,
    workspace_id: &SpaceId,
    hold: &mut Option<OwnedRwLockReadGuard<()>>,
) -> Result<Grant> {
    if !manager.restore_waits_for_update(workspace_id, link.execution()) {
        return manager.admit_agent_restore(link, workspace_id).await;
    }
    hold.take();
    let grant = manager.admit_agent_restore(link, workspace_id).await?;
    *hold = Some(manager.gates.owned_operation().await);
    Ok(grant)
}

fn restore_failed(error: anyhow::Error) -> Action {
    log!("agent recovery cancelled: {error:#}");
    Action::Stop(Cancel::because(format!(
        "automatic restore failed: {error:#}"
    )))
}

fn cancelled_as_done() -> Action {
    Action::Stop(Cancel::because("the assignment is done"))
}

/// How often a waiting restore checks again for survivors it could not stop.
const SURVIVOR_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Wait until none of the stopped agent's processes survive, since a restore
/// must not start beside them, telling the user and the wrapper's terminal
/// once what it waits for and a restart's requests what it waits for now.
async fn await_stopped(
    manager: &Manager,
    execution_id: &ExecutionId,
    workspace_id: &SpaceId,
    stopped: Pause,
    notices: Option<&mut OwnedWriteHalf>,
) -> Result<()> {
    let Pause::Unproven(issue) = stopped else {
        return Ok(());
    };
    manager.notify_restore_waits(execution_id, &issue).await;
    tell_restore_waits(notices, issue).await;
    loop {
        tokio::time::sleep(SURVIVOR_POLL_INTERVAL).await;
        match manager
            .pause_agent_execution(execution_id, workspace_id, Duration::ZERO)
            .await?
        {
            Pause::Paused => return Ok(()),
            Pause::Unproven(issue) => manager.restart_waits(execution_id, &issue).await,
        }
    }
}

/// Tell a waiting wrapper whose release reads it what `issue` holds its
/// restore back, for its terminal.
async fn tell_restore_waits(notices: Option<&mut OwnedWriteHalf>, issue: String) {
    if let Some(wrapper) = notices {
        // A lost connection ends the wait through the wrapper's events.
        let _ = protocol::write(wrapper, &Control::RestoreWaits { issue }).await;
    }
}

/// A protection stop without automatic recovery still ends what the agent left
/// running outside its process group, so `shoal restore` can restore it.
pub(super) async fn stop_survivors(
    manager: &Manager,
    execution_id: &ExecutionId,
    workspace_id: &SpaceId,
) {
    if !manager.agent_was_overloaded(execution_id).await {
        return;
    }
    match manager
        .pause_agent_execution(execution_id, workspace_id, timing::PROCESS_PROOF_TIMEOUT)
        .await
    {
        Ok(Pause::Paused) => {}
        Ok(Pause::Unproven(issue)) => {
            log!("the stopped agent's processes are not proven stopped: {issue}")
        }
        Err(error) => log!("stopped agent's processes not checked: {error:#}"),
    }
}

/// A completed execution no longer needs the restore record its protection
/// stop left; removal is best effort.
pub(super) fn discard_handoff(
    manager: &Manager,
    workspace_id: &SpaceId,
    execution_id: &ExecutionId,
) {
    let record = recovery::record_path(&manager.paths, workspace_id, execution_id);
    if let Err(error) = recovery::consume(&record) {
        log!("unused overload recovery handoff not removed: {error:#}");
    }
}

/// How a stop, such as `shoal stop`, cancelled the restore.
async fn requested_cancel(manager: &Manager, execution_id: &ExecutionId) -> Cancel {
    Cancel::requested(manager.executions.stop_request(execution_id).await)
}

/// Resolves once a reload disables overload recovery, so a waiting agent
/// finishes with its restore record instead of waiting for load to recover.
async fn recovery_disabled(manager: &Manager) {
    let mut config = manager.config_state.watch();
    if config
        .wait_for(|config| !config.machine.overload.recovery.enabled)
        .await
        .is_err()
    {
        std::future::pending::<()>().await;
    }
}

async fn prepare_resume(
    resumes: Resumes<'_>,
    grant: Grant,
    protection: &Protection,
) -> Result<Action> {
    if (resumes.manager)
        .done_cancels_restore(resumes.workspace_id, protection)
        .await?
    {
        return Ok(cancelled_as_done());
    }
    resumes.resume(Some(grant)).await
}

/// What resumes the stopped agent of `execution_id` in the space
/// `workspace_id`, which its wrapper runs under `runs_under`, restoring it
/// under another narrowing when `takes_narrowing`.
#[derive(Clone, Copy)]
struct Resumes<'a> {
    manager: &'a Manager,
    execution_id: &'a ExecutionId,
    workspace_id: &'a SpaceId,
    runs_under: Option<PermissionsConfig>,
    takes_narrowing: bool,
}

impl Resumes<'_> {
    /// Resume the stopped agent with the workspace's ports, under the
    /// narrowing its saved record gives it, unless a stop cancelled its
    /// restore. `grant` is what admitted an automatic restore.
    async fn resume(self, grant: Option<Grant>) -> Result<Action> {
        let Self {
            manager,
            execution_id,
            workspace_id,
            ..
        } = self;
        let ports = manager
            .leases
            .list_ports(manager, Some(&SpaceSelector::id(workspace_id)))
            .await?;
        let (claim, narrowing) = self.saved_narrowing().await?;
        if !manager.resume_agent_execution(execution_id, grant).await? {
            return Ok(Action::Stop(requested_cancel(manager, execution_id).await));
        }
        if let Some(narrowing) = narrowing {
            manager.give_agent_narrowing(execution_id, narrowing).await;
        }
        Ok(Action::Resume(Resumed {
            ports,
            narrowing,
            claim,
        }))
    }

    /// The claim on the agent's saved record, when it saved one, and the
    /// narrowing the record gives it when that differs from the one it ran
    /// with, which only a wrapper that `takes_narrowing` restores it
    /// under; an older one leaves it to a new wrapper.
    async fn saved_narrowing(
        self,
    ) -> Result<(Option<recovery::RecoveryClaim>, Option<PermissionsConfig>)> {
        let path = recovery::record_path(&self.manager.paths, self.workspace_id, self.execution_id);
        let Some((claim, record)) = claim_saved(&path).await? else {
            return Ok((None, None));
        };
        let narrowing = (self.runs_under)
            .filter(|ran| *ran != record.narrowing)
            .map(|_| record.narrowing);
        ensure!(
            narrowing.is_none() || self.takes_narrowing,
            "the agent's wrapper runs an earlier Shoal, which cannot restore it under the permissions it was given since; shoal restore {} restores it in a new one",
            self.workspace_id
        );
        Ok((Some(claim), narrowing))
    }
}

/// How often a resume tries to claim a saved record that a permission
/// change holds for the moment it rewrites it.
const CLAIM_ATTEMPTS: u32 = 20;

/// Claim the saved record at `path` and read it, unless the wrapper saved
/// none.
async fn claim_saved(
    path: &std::path::Path,
) -> Result<Option<(recovery::RecoveryClaim, recovery::Record)>> {
    let mut attempts = 1;
    loop {
        if !path.exists() {
            return Ok(None);
        }
        match recovery::claim_record(path) {
            Ok(claimed) => return Ok(Some(claimed)),
            Err(_) if attempts < CLAIM_ATTEMPTS => attempts += 1,
            Err(error) => return Err(error.context("claim the agent's saved record")),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
