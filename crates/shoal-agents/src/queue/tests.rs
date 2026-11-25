//! The queue's admission, restores and launches over the stand-in.
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};

use anyhow::Result;
use rusqlite::params;
use shoal_daemon_base::alerts::AlertKind;
use shoal_ids::{execution_ids::ExecutionId, queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::{
    config::permissions::{PermissionChange, PermissionsConfig},
    launches::LaunchSource,
    space_names::SpaceSelector,
};

use super::{Grant, OneShotStop, Serving, dispatch::START_GRACE, slots::Slot};
use crate::{
    capacity::Activity,
    executions::ExecutionKind,
    model::{HeldBy, QueueAdmission, QueueEntry, QueueRequest, Workspace},
    overload::Pressure,
    queued_starts::fake::STARTED,
    stand_in::Fixture,
    state::{ExecutionState, QueueKind, QueueState, WorkspaceState},
    stopped_work::PermissionTargets,
};

/// Limit the machine to `limit` agents; host disk must not decide the test.
pub(crate) fn limit_globally(fixture: &Fixture, limit: u32) {
    fixture.configure(&format!(
        "[machine]\nmax_heavy_agents = {limit}\n[machine.overload.disk]\nenabled = false"
    ));
}

/// The launch of an agent in a new workspace named `name`.
pub(crate) async fn request(fixture: &Fixture, name: &str, now: bool) -> QueueRequest {
    launch_in(&fixture.workspace(name).await, now)
}

/// The launch of an agent in `workspace`, which the daemon starts.
pub(crate) fn launch_in(workspace: &Workspace, now: bool) -> QueueRequest {
    QueueRequest {
        source: None,
        replace: false,
        issue_labels: Vec::new(),
        narrowing: Default::default(),
        mode: None,
        name: workspace.name.clone(),
        agent: "claude".into(),
        issue_title: None,
        plan: "{}".into(),
        env: BTreeMap::from([("HERDR_ENV".into(), "1".into())]),
        now,
        space: SpaceSelector::id(&workspace.id),
        starts_in_caller: false,
        blockers: Vec::new(),
    }
}

/// Queue the launch of an agent in a new workspace named `name`.
pub(crate) async fn add(fixture: &Fixture, name: &str) -> QueueAdmission {
    let request = request(fixture, name, false).await;
    (fixture.agents.queue_add(&*fixture.daemon, request))
        .await
        .unwrap()
}

/// Whether `admission` admitted its launch, which the daemon starts.
pub(crate) fn admitted(admission: &QueueAdmission) -> bool {
    (admission.entry.as_ref()).is_some_and(|entry| entry.state == QueueState::Starting)
}

/// Whether the daemon started `id`, passing the queuing terminal's Herdr variables.
pub(crate) fn started(id: &QueueEntryId) -> bool {
    STARTED.lock().unwrap().iter().any(|(started, env)| {
        id == started && env.get("HERDR_ENV").map(String::as_str) == Some("1")
    })
}

/// What the alerts of `kind` told.
pub(crate) fn told(fixture: &Fixture, kind: AlertKind) -> Vec<String> {
    let notified = fixture.daemon.notified.lock().unwrap();
    (notified.iter())
        .filter(|(told, _)| *told == kind)
        .map(|(_, message)| message.clone())
        .collect()
}

/// The ID of the queue entry named `name`, as tests name the entries they
/// queue, since a name never selects one, else the ID of the space of
/// that name, else `name` itself, which selects nothing.
pub(crate) async fn queued(fixture: &Fixture, name: &str) -> String {
    let entries = fixture.agents.queue_entries(&*fixture.daemon).await;
    let entries = entries.unwrap();
    if let Some(entry) = entries.into_iter().find(|entry| entry.name == name) {
        return entry.id.to_string();
    }
    let spaces = fixture.daemon.spaces.lock().unwrap();
    let space = spaces.iter().find(|space| space.name == name);
    space.map_or_else(|| name.to_owned(), |space| space.id.to_string())
}

/// Let every entry's start grace period pass, as for a worker that ended
/// before it took its entry.
pub(crate) async fn abandon_starts(fixture: &Fixture) {
    let past = crate::time::UnixTime::now() - START_GRACE - Duration::from_secs(1);
    (fixture.agents.store)
        .run(move |db| {
            db.execute("UPDATE agent_queue SET seen_at=?1", [past])?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The slots the running and stopped work occupies.
pub(crate) async fn occupied(fixture: &Fixture) -> Vec<Slot> {
    (fixture.agents)
        .occupied_slots(&*fixture.daemon, &HashSet::new(), &Default::default())
        .await
        .unwrap()
}

/// An agent connected in `workspace`, which protection may stop, and the
/// receiver that keeps its connection open.
pub(crate) async fn connected_agent(
    fixture: &Fixture,
    workspace: &Workspace,
) -> (ExecutionId, tokio::sync::watch::Receiver<bool>) {
    let started = (fixture.agents)
        .begin_execution(
            &*fixture.daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    (fixture.agents)
        .track_agent(
            &*fixture.daemon,
            &started.plan.id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            true,
            Duration::from_secs(3600),
        )
        .await;
    (started.plan.id, started.stop)
}

/// A hang guard, not a deadline: the awaited step is already admitted.
pub(crate) async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("queue step hung")
}

/// Agents connected in `workspaces`, each stopped by memory protection with
/// its restore queued in order.
pub(crate) async fn stopped_agents(
    fixture: &Fixture,
    workspaces: &[&Workspace],
) -> Vec<(ExecutionId, tokio::sync::watch::Receiver<bool>)> {
    let mut agents = Vec::new();
    for workspace in workspaces {
        agents.push(connected_agent(fixture, workspace).await);
    }
    for (id, _) in &agents {
        let daemon = &*fixture.daemon;
        assert!(
            fixture
                .agents
                .stop_for_overload(daemon, Pressure::Memory)
                .await
        );
        fixture
            .agents
            .queue_agent_restore(daemon, id)
            .await
            .unwrap();
    }
    agents
}

/// The restore of `execution` waiting for admission over its wrapper's link,
/// once the link counts.
pub(crate) async fn await_restore(
    fixture: &Fixture,
    execution: &ExecutionId,
) -> tokio::task::JoinHandle<Result<Grant>> {
    let waiting = {
        let (agents, execution) = (fixture.agents.clone(), execution.to_owned());
        tokio::spawn(async move {
            let link = agents.agent_queue().link_restore(&execution);
            agents.await_restore_admission(&link).await
        })
    };
    bounded(async {
        while !(fixture.agents.agent_queue())
            .restore_links()
            .contains(execution)
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    waiting
}

/// A process group ID above every system's PID limit, so no process has it.
const ABSENT_GROUP: u32 = i32::MAX as u32;

pub(crate) fn publish_recovery(fixture: &Fixture) {
    let epoch = fixture.agents.overload_recovery().epoch();
    fixture.agents.overload_recovery().publish(Some(epoch));
}

/// Each entry's name, kind and state in queue order, without the failed
/// launches, which wait for nothing.
pub(crate) async fn queue(fixture: &Fixture) -> Vec<(String, QueueKind, QueueState)> {
    live(fixture)
        .await
        .into_iter()
        .map(|entry| (entry.name.into(), entry.kind, entry.state))
        .collect()
}

/// The entries that wait or start, without the failed launches.
pub(crate) async fn live(fixture: &Fixture) -> Vec<QueueEntry> {
    let mut entries = (fixture.agents.queue_entries(&*fixture.daemon))
        .await
        .unwrap();
    entries.retain(|entry| entry.state != QueueState::Failed);
    entries
}

/// Run a dispatch pass.
pub(crate) async fn dispatch(fixture: &Fixture) {
    fixture
        .agents
        .dispatch_queue(&*fixture.daemon)
        .await
        .unwrap();
}

/// Discard a stopped agent as `shoal restore --discard-agent` does once its wrapper ended.
async fn forget(fixture: &Fixture, workspace: &Workspace, execution: &ExecutionId) {
    // No marked process belongs to the agent. A host scan would also count
    // every same-user process whose environment it cannot read, such as
    // ssh-agent on Linux, as a survivor and keep the slot.
    (fixture.agents)
        .finish_execution_after_scan(
            &*fixture.daemon,
            execution.to_owned(),
            ExecutionKind::Command,
            Some(0),
            Some(Ok(crate::process::identity::Scan::default())),
        )
        .await
        .unwrap();
    std::fs::remove_file(crate::execution::recovery::record_path(
        &fixture.agents.paths,
        &workspace.id,
        execution,
    ))
    .unwrap();
}

use QueueKind::{Launch, Restore};
use QueueState::{Queued, Starting};

#[tokio::test]
async fn a_full_machine_queues_until_a_start_ends() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let daemon = &*fixture.daemon;

    let first = add(&fixture, "first").await.entry.unwrap();
    assert_eq!(first.state, QueueState::Starting);
    let second = add(&fixture, "second").await;
    assert!(!admitted(&second));
    assert_eq!(second.waiting, 1);
    let second = second.entry.unwrap();
    assert_eq!(
        (second.state, second.position),
        (QueueState::Queued, Some(1))
    );
    // Starting now takes a slot beyond the limit without passing the queue.
    let urgent = request(&fixture, "urgent", true).await;
    let now = fixture.agents.queue_add(daemon, urgent).await.unwrap();
    assert!(admitted(&now));

    (fixture
        .agents
        .fail_queue_entry(daemon, &first.id, "setup failed"))
    .await
    .unwrap();
    (fixture
        .agents
        .fail_queue_entry(daemon, &now.entry.unwrap().id, "gone"))
    .await
    .unwrap();
    dispatch(&fixture).await;
    assert!(started(&second.id));
    let entries = live(&fixture).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].state, QueueState::Starting);
    let failures = told(&fixture, AlertKind::StartFailed);
    assert!(
        failures
            .iter()
            .any(|message| message.contains("setup failed"))
    );
    assert!(!told(&fixture, AlertKind::QueueStarted).is_empty());
}

#[tokio::test]
async fn repository_limits_hold_back_only_their_own_entries() {
    let fixture = Fixture::new().await;
    let limited = fixture.repository_named("limited").await;
    let other = fixture.repository_named("other").await;
    limit_globally(&fixture, 3);
    fixture.configure_repository(&limited.id, "agent.max_heavy_running = 1\n");
    let add = async |repository, name| {
        let workspace = fixture.workspace_in(repository, name).await;
        let request = launch_in(&workspace, false);
        let admission = fixture.agents.queue_add(&*fixture.daemon, request).await;
        admitted(&admission.unwrap())
    };

    assert!(add(&limited, "one").await);
    assert!(!add(&limited, "two").await);
    assert!(add(&other, "three").await);
    assert!(add(&other, "four").await);
    // The machine is full now, so other repositories wait too.
    assert!(!add(&other, "five").await);
    let positions: Vec<_> = (fixture.agents.queue_entries(&*fixture.daemon))
        .await
        .unwrap()
        .into_iter()
        .filter_map(|entry| entry.position.map(|position| (entry.name, position)))
        .collect();
    assert_eq!(positions, [("two".into(), 1), ("five".into(), 2)]);
}

#[tokio::test]
async fn abandoned_starts_expire_after_the_grace_period_unless_held() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 2);
    let held = add(&fixture, "held").await.entry.unwrap();
    add(&fixture, "abandoned").await;
    fixture.agents.hold_queue_entry(&held.id).await.unwrap();
    abandon_starts(&fixture).await;

    dispatch(&fixture).await;
    let remaining: Vec<_> = (live(&fixture).await.into_iter())
        .map(|entry| entry.id)
        .collect();
    assert_eq!(remaining, std::slice::from_ref(&held.id));
    let failures = told(&fixture, AlertKind::StartFailed);
    assert!(failures.iter().any(|message| message.contains("abandoned")));
    // Releasing the hold starts a fresh grace period instead of ending it.
    fixture.agents.release_queue_hold(&held.id).await.unwrap();
    dispatch(&fixture).await;
    assert!(fixture.agents.queue_entry_starting(&held.id).await.unwrap());
}

#[tokio::test]
async fn failing_an_ended_entry_frees_nothing_and_says_nothing() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let daemon = &*fixture.daemon;
    let starting = add(&fixture, "starting").await.entry.unwrap();
    (fixture
        .agents
        .fail_queue_entry(daemon, &starting.id, "first"))
    .await
    .unwrap();
    // Removing its workspace also ends an entry before its launch reports.
    (fixture
        .agents
        .fail_queue_entry(daemon, &starting.id, "second"))
    .await
    .unwrap();
    assert_eq!(told(&fixture, AlertKind::StartFailed).len(), 1);
}

#[tokio::test]
async fn only_waiting_entries_can_be_removed() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let daemon = &*fixture.daemon;
    let starting = add(&fixture, "starting").await.entry.unwrap();
    let waiting = add(&fixture, "waiting").await.entry.unwrap();
    // Only a start can fail; a waiting entry stays queued.
    let failed = fixture
        .agents
        .fail_queue_entry(daemon, &waiting.id, "not started");
    assert!(failed.await.is_err());
    assert_eq!(fixture.agents.queue_entries(daemon).await.unwrap().len(), 2);

    (fixture.agents.queue_remove(daemon, &starting.name, None))
        .await
        .unwrap_err();
    let waiting_id = queued(&fixture, "waiting").await;
    let removed = fixture.agents.queue_remove(daemon, &waiting_id, None).await;
    assert_eq!(removed.unwrap().name, "waiting");
    let again = fixture.agents.queue_remove(daemon, &waiting_id, None).await;
    assert!(again.is_err());
    assert!(fixture.agents.queued_plan(&starting.id).await.is_ok());
}

#[tokio::test]
async fn each_agent_execution_uses_its_own_slot() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 2);
    let workspace = fixture.workspace("busy").await;
    let id = workspace.id.clone();
    // Two agents await reattachment in one workspace after a daemon restart.
    (fixture.agents.store)
        .run(move |db| {
            for execution in ["first", "second"] {
                db.execute(
                    "INSERT INTO executions(id,space_id,state) VALUES (?1,?2,?3)",
                    params![execution, id, ExecutionState::Unknown],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    // A resume record of the same execution is the same agent.
    let record = crate::execution::recovery::record_path(
        &fixture.agents.paths,
        &workspace.id,
        &ExecutionId::from("first"),
    );
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, "{\"agent\":\"claude\"}").unwrap();
    assert_eq!(occupied(&fixture).await.len(), 2);
    assert!(!admitted(&add(&fixture, "waiting").await));
}

#[tokio::test]
async fn a_ci_jobs_runner_awaiting_reattachment_holds_no_agent_slot() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("ci-rust-4").await;
    let id = workspace.id.clone();
    // A job's runner awaits reattachment after a daemon restart.
    (fixture.agents.store)
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state) VALUES ('runner',?1,?2)",
                params![id, ExecutionState::Unknown],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (fixture.records.job_workspaces.lock().unwrap()).insert(workspace.id.clone());
    assert!(occupied(&fixture).await.is_empty());
    assert!(admitted(&add(&fixture, "agent").await));
}

#[tokio::test]
async fn low_disk_holds_fixed_limits() {
    let fixture = Fixture::new().await;
    // No disk has this much free space, so the host reads low or critical;
    // either holds every start after the first below the fixed limit.
    fixture.configure(
        "[machine]\nmax_heavy_agents = 3\n[machine.overload.warning]\ndisk_free_gib = 1048576",
    );
    add(&fixture, "first").await;
    assert!(!admitted(&add(&fixture, "second").await));
    // Starting now still starts at once.
    let urgent = request(&fixture, "urgent", true).await;
    let admission = fixture.agents.queue_add(&*fixture.daemon, urgent).await;
    assert!(admitted(&admission.unwrap()));
}

#[tokio::test]
async fn protection_stopped_agents_keep_their_slot_without_load() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("stopped").await;
    let agent = connected_agent(&fixture, &workspace).await;
    let activities = async || -> Vec<Activity> {
        (occupied(&fixture).await.into_iter())
            .map(|slot| slot.activity)
            .collect()
    };
    assert!(matches!(activities().await[..], [Activity::Running { .. }]));
    let daemon = &*fixture.daemon;
    assert!(
        fixture
            .agents
            .stop_for_overload(daemon, Pressure::Memory)
            .await
    );
    // Until its processes are proven stopped the agent's load may still show.
    assert!(matches!(activities().await[..], [Activity::Running { .. }]));
    (fixture.agents.queue_agent_restore(daemon, &agent.0))
        .await
        .unwrap();
    // Without a turn report the agent counts as interrupted.
    assert!(!fixture.agents.queue_entries(daemon).await.unwrap()[0].waits_for_update);
    let _restoring = await_restore(&fixture, &agent.0).await;
    assert_eq!(activities().await, [Activity::Stopped]);
}

#[tokio::test]
async fn stopped_agents_restore_in_queue_order_before_later_launches() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 2);
    let (first, second) = (
        fixture.workspace("first").await,
        fixture.workspace("second").await,
    );
    let agents = stopped_agents(&fixture, &[&first, &second]).await;
    let launch = add(&fixture, "later").await;
    assert!(!admitted(&launch));
    let launch = launch.entry.unwrap();
    let restoring = [
        await_restore(&fixture, &agents[0].0).await,
        await_restore(&fixture, &agents[1].0).await,
    ];
    dispatch(&fixture).await;
    assert_eq!(
        queue(&fixture).await,
        [
            ("first".into(), Restore, Queued),
            ("second".into(), Restore, Queued),
            ("later".into(), Launch, Queued),
        ]
    );

    // A healthy interval restores the first stopped agent only.
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    let [first_restore, second_restore] = restoring;
    bounded(first_restore).await.unwrap().unwrap();
    assert_eq!(fixture.agents.overload_recovery().published(), None);
    assert_eq!(
        queue(&fixture).await[..2],
        [
            ("first".into(), Restore, Starting),
            ("second".into(), Restore, Queued),
        ]
    );
    // The restored agent registers and takes over the slot it kept. Its group
    // must not exist, or forgetting it later finds survivors and keeps the slot.
    let register = |execution: &ExecutionId| {
        (fixture.agents).record_execution_child(execution.to_owned(), None, ABSENT_GROUP)
    };
    register(&agents[0].0).await.unwrap();
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    bounded(second_restore).await.unwrap().unwrap();
    register(&agents[1].0).await.unwrap();
    // Both agents have their slots back, so the launch keeps waiting.
    dispatch(&fixture).await;
    assert_eq!(queue(&fixture).await, [("later".into(), Launch, Queued)]);
    forget(&fixture, &first, &agents[0].0).await;
    dispatch(&fixture).await;
    assert!(started(&launch.id));
}

#[tokio::test]
async fn a_stop_after_admission_returns_the_restore_to_its_place() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 2);
    let (first, second) = (
        fixture.workspace("first").await,
        fixture.workspace("second").await,
    );
    let agents = stopped_agents(&fixture, &[&first, &second]).await;
    let daemon = &*fixture.daemon;
    let restoring = await_restore(&fixture, &agents[0].0).await;
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    let epoch = bounded(restoring).await.unwrap().unwrap();
    // Load worsens again while the restore checks its workspace.
    fixture.agents.overload_recovery().reset();
    let confirmed = fixture
        .agents
        .confirm_restore(daemon, &agents[0].0, &first.id, epoch);
    assert!(!confirmed.await.unwrap());
    assert_eq!(
        queue(&fixture).await,
        [
            ("first".into(), Restore, Queued),
            ("second".into(), Restore, Queued),
        ]
    );
    // The next healthy interval admits it again, and that admission holds.
    let restoring = await_restore(&fixture, &agents[0].0).await;
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    let epoch = bounded(restoring).await.unwrap().unwrap();
    let confirmed = fixture
        .agents
        .confirm_restore(daemon, &agents[0].0, &first.id, epoch);
    assert!(confirmed.await.unwrap());
}

#[tokio::test]
async fn admission_consumes_only_the_published_recovery_epoch() {
    let fixture = Fixture::new().await;
    let recovery = fixture.agents.overload_recovery();
    publish_recovery(&fixture);
    let published = recovery.published().unwrap();
    // A stop between the check and the admission withdraws the readings.
    recovery.reset();
    assert_eq!(recovery.consume(published), None);
    publish_recovery(&fixture);
    let published = recovery.published().unwrap();
    assert_eq!(recovery.consume(published), Some(published + 1));
    assert_eq!(recovery.published(), None);
}

#[tokio::test]
async fn lowered_limits_hold_restores_until_a_stopped_agent_is_forgotten() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 2);
    let (first, second) = (
        fixture.workspace("first").await,
        fixture.workspace("second").await,
    );
    let agents = stopped_agents(&fixture, &[&first, &second]).await;
    let restoring = await_restore(&fixture, &agents[0].0).await;
    limit_globally(&fixture, 1);
    publish_recovery(&fixture);
    // Each restore would take back its own slot, but the other one fills the machine.
    dispatch(&fixture).await;
    assert_eq!(queue(&fixture).await[0], ("first".into(), Restore, Queued));
    assert!(fixture.agents.overload_recovery().published().is_some());

    // `shoal stop` cancels the second restore; its wrapper ends with the
    // recovery record, which still holds the slot until it is forgotten.
    fixture.agents.remove_restore(&agents[1].0).await;
    (fixture.agents)
        .finish_execution(
            &*fixture.daemon,
            agents[1].0.clone(),
            ExecutionKind::Command,
            Some(0),
        )
        .await
        .unwrap();
    dispatch(&fixture).await;
    assert_eq!(queue(&fixture).await, [("first".into(), Restore, Queued)]);
    std::fs::remove_file(crate::execution::recovery::record_path(
        &fixture.agents.paths,
        &second.id,
        &agents[1].0,
    ))
    .unwrap();
    dispatch(&fixture).await;
    bounded(restoring).await.unwrap().unwrap();
    assert_eq!(queue(&fixture).await, [("first".into(), Restore, Starting)]);
}

#[tokio::test]
async fn restart_keeps_restores_in_order_without_starting_one_twice() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 2);
    let (first, second) = (
        fixture.workspace("first").await,
        fixture.workspace("second").await,
    );
    let agents = stopped_agents(&fixture, &[&first, &second]).await;
    let launch = add(&fixture, "later").await.entry.unwrap();
    let restoring = await_restore(&fixture, &agents[0].0).await;
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    bounded(restoring).await.unwrap().unwrap();

    // The daemon restarts before the restored agent registers.
    let restarted = fixture.restart().await;
    limit_globally(&restarted, 2);
    restarted.agents.refresh_starting_entries().await.unwrap();
    publish_recovery(&restarted);
    // Until the waiting wrappers reattach, nothing restores and the launch
    // behind the restores waits for its slot.
    dispatch(&restarted).await;
    assert!(!started(&launch.id));
    assert_eq!(
        queue(&restarted).await,
        [
            ("first".into(), Restore, Starting),
            ("second".into(), Restore, Queued),
            ("later".into(), Launch, Queued),
        ]
    );
    // The first wrapper never got its restore, so it reattaches waiting and
    // keeps its place instead of queueing a second restore.
    (restarted.agents)
        .queue_restore(
            &*restarted.daemon,
            &agents[0].0,
            &"claude".into(),
            &first.id,
        )
        .await
        .unwrap();
    let restoring = await_restore(&restarted, &agents[0].0).await;
    dispatch(&restarted).await;
    bounded(restoring).await.unwrap().unwrap();
    assert_eq!(
        queue(&restarted).await,
        [
            ("first".into(), Restore, Starting),
            ("second".into(), Restore, Queued),
            ("later".into(), Launch, Queued),
        ]
    );
    // The second wrapper never reattaches. Once its execution ends, its
    // agent holds no slot, which the launch takes, and the daemon takes
    // over its restore, which waits for healthy readings and a free slot.
    let gone = agents[1].0.clone();
    (restarted.agents.store)
        .run(move |db| {
            db.execute("DELETE FROM executions WHERE id=?1", [gone])?;
            Ok(())
        })
        .await
        .unwrap();
    dispatch(&restarted).await;
    assert!(started(&launch.id));
    publish_recovery(&restarted);
    dispatch(&restarted).await;
    assert_eq!(
        queue(&restarted).await[1..],
        [
            ("later".into(), Launch, Starting),
            ("second".into(), Restore, Queued),
        ]
    );
    let restores = restarted.agents.queued_stopped_restores().await.unwrap();
    assert!(restores.contains(&agents[1].0));
}

#[tokio::test]
async fn dynamic_admission_restores_beside_a_running_agent() {
    let fixture = Fixture::new().await;
    fixture.configure(
        "[machine.overload.memory]\nenabled = false\n[machine.overload.disk]\nenabled = false",
    );
    let (busy, stopped) = (
        fixture.workspace("busy").await,
        fixture.workspace("stopped").await,
    );
    let agents = stopped_agents(&fixture, &[&stopped]).await;
    // A settled agent keeps running beside the stopped one; memory, not
    // CPU, decides whether the restore fits.
    let _running = connected_agent(&fixture, &busy).await;
    let restoring = await_restore(&fixture, &agents[0].0).await;
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    assert_eq!(queue(&fixture).await[0].2, Starting);
    bounded(restoring).await.unwrap().unwrap();
}

#[tokio::test]
async fn a_launch_in_an_existing_workspace_its_caller_starts_is_left_to_it() {
    let fixture = Fixture::new().await;
    fixture
        .configure("[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false");
    let workspace = fixture.workspace("here").await;
    let request = QueueRequest {
        starts_in_caller: true,
        ..launch_in(&workspace, false)
    };
    let admission = fixture.agents.queue_add(&*fixture.daemon, request).await;
    // The entry keeps the workspace's one launch while the caller starts it.
    let admission = admission.unwrap();
    let entry = admission.entry.unwrap();
    assert!(admission.start);
    assert_eq!(entry.opens.as_ref(), Some(&workspace.id));
    assert!(!started(&entry.id));
}

#[tokio::test]
async fn a_launch_in_an_existing_workspace_started_now_starts_beyond_the_limit() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let child = fixture.workspace("child").await;
    assert!(admitted(&add(&fixture, "busy").await));
    let admission = fixture
        .agents
        .queue_add(&*fixture.daemon, launch_in(&child, true))
        .await
        .unwrap();
    let entry = admission.entry.unwrap();
    assert!(!admission.start);
    assert_eq!(entry.state, QueueState::Starting);
    assert!(started(&entry.id));
}

#[tokio::test]
async fn an_expired_launch_in_an_existing_workspace_reports_its_failure() {
    let fixture = Fixture::new().await;
    fixture
        .configure("[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false");
    let child = fixture.workspace("child").await;
    let entry = fixture
        .agents
        .queue_add(&*fixture.daemon, launch_in(&child, false))
        .await
        .unwrap()
        .entry
        .unwrap();
    // Its worker ended, as on a restart, before opening the workspace.
    abandon_starts(&fixture).await;
    dispatch(&fixture).await;
    assert!(live(&fixture).await.is_empty());
    let failures = told(&fixture, AlertKind::StartFailed);
    assert!(
        (failures.iter()).any(|message| message.contains(entry.name.as_str())),
        "{failures:?}"
    );
}

/// Admission hands a restore to its wrapper's link whether or not the
/// wrapper waits for it at that moment, and a revoked one returns to its
/// place on the same link.
#[tokio::test]
async fn a_wrappers_link_carries_its_restore_without_a_wait_in_flight() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let daemon = &*fixture.daemon;
    let loaded = fixture.workspace("loaded").await;
    let (execution, _stop) = connected_agent(&fixture, &loaded).await;
    assert!(
        fixture
            .agents
            .stop_for_overload(daemon, Pressure::Memory)
            .await
    );
    (fixture.agents.queue_agent_restore(daemon, &execution))
        .await
        .unwrap();
    let link = fixture.agents.agent_queue().link_restore(&execution);
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    assert_eq!(
        queue(&fixture).await,
        [("loaded".into(), Restore, Starting)]
    );
    let grant = bounded(fixture.agents.await_restore_admission(&link)).await;
    let grant = grant.unwrap();
    assert!(matches!(grant, Grant::Recovery(_)), "{grant:?}");

    // A stop since the readings revokes it, and the link waits again.
    fixture.agents.overload_recovery().reset();
    let confirmed = (fixture.agents).confirm_restore(daemon, &execution, &loaded.id, grant);
    assert!(!confirmed.await.unwrap());
    assert_eq!(queue(&fixture).await, [("loaded".into(), Restore, Queued)]);
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    let grant = bounded(fixture.agents.await_restore_admission(&link)).await;
    assert!(matches!(grant.unwrap(), Grant::Recovery(_)));
}

#[tokio::test]
async fn the_agent_that_takes_a_launchs_entry_learns_what_made_the_launch() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let source = LaunchSource::Trigger {
        name: "nightly".into(),
    };
    let workspace = fixture.workspace("launch").await;
    let launch = QueueRequest {
        source: Some(source.clone()),
        ..launch_in(&workspace, false)
    };
    let entry = fixture.agents.queue_add(daemon, launch).await;
    let entry = entry.unwrap().entry.unwrap();
    (fixture.agents)
        .queue_attach(daemon, &entry.id, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let consume = || fixture.agents.consume_queue_entry(&entry.id, &workspace.id);
    assert_eq!(consume().await, Some(source));
    // The entry is gone, so taking it again learns nothing.
    assert_eq!(consume().await, None);
}

#[tokio::test]
async fn a_source_this_build_cannot_read_leaves_the_entry_consumed() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("launch").await;
    let entry = (fixture
        .agents
        .queue_add(daemon, launch_in(&workspace, false)))
    .await;
    let entry = entry.unwrap().entry.unwrap();
    (fixture.agents)
        .queue_attach(daemon, &entry.id, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let id = entry.id.clone();
    (fixture.agents.store)
        .run(move |db| {
            let unknown = r#"{"kind":"webhook"}"#;
            db.execute(
                "UPDATE agent_queue SET source=?2 WHERE id=?1",
                params![id, unknown],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let consumed = (fixture.agents.consume_queue_entry(&entry.id, &workspace.id)).await;
    assert_eq!(consumed, None);
    assert_eq!(
        fixture.agents.started_launch(&entry.id).await.unwrap(),
        Some(workspace.id.clone())
    );
}

/// What one-shot runs of a workspace ranking finds none of serve.
fn reviewed() -> Serving {
    Serving {
        space: String::from("reviewed").into(),
        queued_after: i64::MAX,
    }
}

#[tokio::test]
async fn protection_stops_oneshot_runs_which_wait_for_recovery_to_run_again() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let fixture = Fixture::new().await;
    let repo = fixture.repository().await;
    limit_globally(&fixture, 2);
    let daemon = &*fixture.daemon;
    let queue = fixture.agents.agent_queue();
    let stops = std::sync::Arc::new(AtomicUsize::new(0));
    let counted = stops.clone();
    let mut run = queue.request_oneshot(&repo.id, reviewed(), false, move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    dispatch(&fixture).await;
    bounded(run.admitted()).await;
    // Protection stops the run before any agent.
    assert!(
        fixture
            .agents
            .stop_for_overload(daemon, Pressure::Memory)
            .await
    );
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    // Neither the stopped run nor a held one is stopped again, so the next
    // stop falls to the agents, of which there are none.
    queue.hold_oneshot(&repo.id);
    assert!(
        !fixture
            .agents
            .stop_for_overload(daemon, Pressure::Memory)
            .await
    );
    assert_eq!(queue.stop_oneshots(OneShotStop::All), 0);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    drop(run);

    let mut again = queue.request_oneshot(&repo.id, reviewed(), true, || {});
    dispatch(&fixture).await;
    shoal_core::test_support::assert_pending(again.admitted()).await;
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    bounded(again.admitted()).await;
}

#[tokio::test]
async fn a_launch_waiting_for_setup_takes_a_permission_change() {
    let fixture = Fixture::new().await;
    fixture
        .configure("[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false");
    let daemon = &*fixture.daemon;
    let child = fixture.workspace("child").await;
    fixture
        .set_state(&child.id, WorkspaceState::Failed, Some("setup failed"))
        .await;
    let launch = |now| launch_in(&child, now);
    // One that would start now needs the workspace ready.
    fixture
        .agents
        .queue_add(daemon, launch(true))
        .await
        .unwrap_err();
    let admission = fixture.agents.queue_add(daemon, launch(false)).await;
    let admission = admission.unwrap();
    let entry = admission.entry.unwrap();
    assert!(!admission.start);
    assert_eq!(entry.state, QueueState::Queued);
    assert_eq!(entry.blockers, vec!["setup of child".to_owned()]);

    let ready = fixture.workspace("ready").await;
    let change = PermissionChange::assigned("merge=direct").unwrap();
    // The agent starts under the narrowing its plan keeps.
    let narrowing = async || {
        let id = entry.id.clone();
        let plan: String = (fixture.agents.store)
            .run(move |db| {
                Ok(
                    db.query_row("SELECT plan FROM agent_queue WHERE id=?1", [id], |row| {
                        row.get(0)
                    })?,
                )
            })
            .await
            .unwrap();
        let plan: serde_json::Value = serde_json::from_str(&plan).unwrap();
        serde_json::from_value::<PermissionsConfig>(plan["launch"]["narrowing"].clone())
            .unwrap_or_default()
    };
    let change_for = async |launches: &[SpaceSelector]| {
        let targets = PermissionTargets {
            agents: &[],
            launches,
        };
        (fixture.agents)
            .change_stopped_permissions(daemon, targets, &change, None)
            .await
    };

    // No agent waits for the setup of a ready workspace, so none changes.
    let both = [SpaceSelector::id(&child.id), SpaceSelector::id(&ready.id)];
    change_for(&both).await.unwrap_err();
    assert_eq!(narrowing().await, PermissionsConfig::default());
    change_for(&both[..1]).await.unwrap();
    assert_eq!(narrowing().await, change.set);
    let entries = fixture.agents.queue_entries(daemon).await.unwrap();
    assert_eq!(entries[0].narrowing, change.set);
}

#[tokio::test]
async fn an_entrys_wait_for_memory_lasts_while_passes_hold_it_for_memory() {
    let fixture = Fixture::new().await;
    let queue = &fixture.agents.agent_queue();
    let id = QueueEntryId::from("waiting");
    let memory = HeldBy::Memory { needed: 2, left: 1 };
    let start = std::time::Instant::now();
    let at = |seconds| start + Duration::from_secs(seconds);
    let unstarted = [id.clone()].into();
    let hold = |held, seconds| {
        queue.record_held_back([(id.clone(), held)].into(), &unstarted, at(seconds));
    };
    hold(memory, 0);
    hold(memory, 60);
    // A pass that started another entry first did not measure it.
    queue.record_held_back(Default::default(), &unstarted, at(70));
    assert_eq!(
        queue.waited_for_memory(&id, at(90)),
        Duration::from_secs(90)
    );
    // Held for anything else, or started, it waits for memory anew.
    hold(HeldBy::MaxHeavyAgents, 120);
    assert_eq!(queue.waited_for_memory(&id, at(120)), Duration::ZERO);
    hold(memory, 180);
    assert_eq!(
        queue.waited_for_memory(&id, at(200)),
        Duration::from_secs(20)
    );
    queue.record_held_back(Default::default(), &Default::default(), at(240));
    assert_eq!(queue.waited_for_memory(&id, at(240)), Duration::ZERO);
}

/// Save a stopped agent of `mode` in the space `id` for its restore, and
/// queue the restore as `shoal resume` does.
pub(crate) async fn queue_saved_restore(fixture: &Fixture, id: &SpaceId, mode: &str) -> QueueEntry {
    let execution = ExecutionId::from(format!("stopped-{mode}"));
    let agent = shoal_protocol::executions::TrackedAgent {
        mode: mode.into(),
        ..shoal_protocol::executions::TrackedAgent::named("claude")
    };
    let paths = &fixture.agents.paths;
    let saved = crate::execution::recovery::save_record(paths, id, &execution, &agent, None, None);
    saved.unwrap();
    let env = BTreeMap::from([("HERDR_ENV".into(), "1".into())]);
    (fixture.agents)
        .queue_stopped_restore(&*fixture.daemon, &SpaceSelector::id(id), &execution, env)
        .await
        .unwrap()
}

/// A chat without a repository takes its weight from the global config's
/// modes: a heavy mode's restore waits for a slot on a full machine, as a
/// workspace agent's does, and a light one's starts beside it.
#[tokio::test]
async fn a_chats_restore_waits_for_a_slot_by_its_modes_weight() {
    let fixture = Fixture::new().await;
    fixture.configure(
        "[machine]\nmax_heavy_agents = 1\n[machine.overload.disk]\nenabled = false\n\
         [modes.solo]\nkind = 'chat'\nrepository = false\n",
    );
    assert!(admitted(&add(&fixture, "busy").await));
    let chat = fixture.chat("master").await;

    let heavy = queue_saved_restore(&fixture, &chat, "solo").await;
    let light = queue_saved_restore(&fixture, &chat, "chat").await;
    assert_eq!(heavy.repository_id, None);
    dispatch(&fixture).await;

    assert!(!started(&heavy.id));
    assert!(started(&light.id));
    let entries = fixture
        .agents
        .queue_entries(&*fixture.daemon)
        .await
        .unwrap();
    let waiting = entries.iter().find(|entry| entry.id == heavy.id).unwrap();
    assert_eq!(waiting.held_by, Some(HeldBy::MaxHeavyAgents));
}

/// A heavy chat's agent holds a machine slot, and one in a repository
/// that repository's.
#[tokio::test]
async fn a_heavy_chats_agent_holds_a_slot() {
    let fixture = Fixture::new().await;
    let repo = fixture.repository().await;
    fixture.configure(
        "[machine]\nmax_heavy_agents = 2\n[machine.overload.disk]\nenabled = false\n\
         [modes.solo]\nkind = 'chat'\nrepository = false\n\
         [modes.pair]\nkind = 'chat'\n",
    );
    fixture.chat_in("light", "chat", Some(&repo)).await;
    fixture.chat_in("solo", "solo", None).await;
    fixture.chat_in("pair", "pair", Some(&repo)).await;
    let daemon = &*fixture.daemon;
    let light_mode = fixture.agents.light(daemon).await.unwrap();
    let slots = (fixture.agents)
        .occupied_slots(daemon, &HashSet::new(), &light_mode)
        .await;
    let mut slots: Vec<_> = (slots.unwrap().into_iter())
        .map(|slot| slot.repository)
        .collect();
    slots.sort();
    assert_eq!(slots, [None, Some(repo.id.clone())]);
    assert!(
        !admitted(&add(&fixture, "held").await),
        "the machine is full"
    );
}

/// A running claude agent in a workspace, whose proxy found claude's
/// upstream rate-limited for an hour; the workspace and when the limit
/// lifts.
async fn limit_claude(fixture: &Fixture) -> (SpaceSelector, crate::time::UnixTime) {
    use shoal_protocol::executions::{ServiceLimit, UpstreamLimit};
    let workspace = fixture.workspace("busy").await;
    let id = workspace.id.clone();
    (fixture.agents.store)
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state,agent) VALUES ('running',?1,?2,'claude')",
                params![id, ExecutionState::Running],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let until = crate::time::UnixTime::now() + Duration::from_secs(3600);
    let selector = SpaceSelector::id(&workspace.id);
    let limit = UpstreamLimit {
        limit: ServiceLimit::RateLimited,
        until: Some(until),
    };
    (fixture.agents)
        .report_upstream_limit(&*fixture.daemon, &selector, "running".into(), Some(limit))
        .await
        .unwrap();
    (selector, until)
}

#[tokio::test]
async fn a_tools_upstream_limit_holds_its_queued_agents_while_others_start() {
    use shoal_protocol::executions::ServiceLimit;
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 3);
    let daemon = &*fixture.daemon;
    let (selector, until) = limit_claude(&fixture).await;
    let claude = add(&fixture, "claude").await;
    assert!(!admitted(&claude));
    let claude = claude.entry.unwrap();
    let codex = QueueRequest {
        agent: "codex".into(),
        ..request(&fixture, "codex", false).await
    };
    assert!(admitted(
        &fixture.agents.queue_add(daemon, codex).await.unwrap()
    ));
    let entries = fixture.agents.queue_entries(daemon).await.unwrap();
    let waiting = entries.iter().find(|entry| entry.id == claude.id).unwrap();
    assert_eq!(
        waiting.held_by,
        Some(HeldBy::UpstreamLimit {
            limit: ServiceLimit::RateLimited,
            until
        })
    );
    // Once its upstream answers again, it starts.
    (fixture
        .agents
        .report_upstream_limit(daemon, &selector, "running".into(), None))
    .await
    .unwrap();
    dispatch(&fixture).await;
    assert!(started(&claude.id));
}

/// An answer through any proxy of the tool lifts its limit, not only one
/// through the proxy that met it, which may have stopped since; another
/// tool's answer leaves it.
#[tokio::test]
async fn any_agent_of_the_tool_that_gets_an_answer_lifts_its_upstream_limit() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 3);
    let (selector, _) = limit_claude(&fixture).await;
    let claude = add(&fixture, "claude").await.entry.unwrap();
    (fixture.agents.store)
        .run(|db| {
            for (id, agent) in [("codex-agent", "codex"), ("other-claude", "claude")] {
                db.execute(
                    "INSERT INTO executions(id,space_id,state,agent)
                     SELECT ?1,space_id,state,?2 FROM executions WHERE id='running'",
                    params![id, agent],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let answered = async |execution: &'static str| {
        (fixture.agents)
            .report_upstream_answered(&*fixture.daemon, &selector, execution.into())
            .await
            .unwrap();
        dispatch(&fixture).await;
    };
    answered("codex-agent").await;
    assert!(!started(&claude.id));
    answered("other-claude").await;
    assert!(started(&claude.id));
}

/// A chat's agent, light or heavy, waits while its tool's upstream stands
/// under a limit, as a workspace agent's does.
#[tokio::test]
async fn a_chats_restore_waits_for_its_tools_upstream_limit() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 3);
    limit_claude(&fixture).await;
    let chat = fixture.chat("master").await;
    let entry = queue_saved_restore(&fixture, &chat, "chat").await;
    dispatch(&fixture).await;
    assert!(!started(&entry.id));
    let entries = fixture
        .agents
        .queue_entries(&*fixture.daemon)
        .await
        .unwrap();
    let waiting = entries.iter().find(|queued| queued.id == entry.id).unwrap();
    assert!(matches!(
        waiting.held_by,
        Some(HeldBy::UpstreamLimit { .. })
    ));
}

#[tokio::test]
async fn an_exit_that_waits_keeps_queued_launches_for_the_next_daemon() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let daemon = &*fixture.daemon;
    let first = add(&fixture, "first").await.entry.unwrap();
    let second = add(&fixture, "second").await.entry.unwrap();
    let held = fixture.agents.gates.hold_back(crate::protocol::Exit::Stop);
    (fixture.agents.fail_queue_entry(daemon, &first.id, "gone"))
        .await
        .unwrap();
    dispatch(&fixture).await;
    let state = async || {
        let entries = fixture.agents.queue_entries(daemon).await.unwrap();
        (entries.into_iter())
            .find(|entry| entry.id == second.id)
            .map(|entry| entry.state)
    };
    assert_eq!(state().await, Some(QueueState::Queued));
    assert!(!started(&second.id));
    drop(held);
    dispatch(&fixture).await;
    assert_eq!(state().await, Some(QueueState::Starting));
    assert!(started(&second.id));
}

mod actions;
mod blockers;
mod claims;
mod failures;
mod setup_waits;
mod stopped;
mod weight;
mod worker;
