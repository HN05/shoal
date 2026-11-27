use crate::execution::recovery::{self, LastSessionExt, record_path};
use crate::protocol::QueuedLaunches;
use crate::{executions::StopRequest, state::QueueKind::Restore};

use super::*;

/// The selector of the space named `name`.
fn named(fixture: &Fixture, name: &str) -> SpaceSelector {
    let spaces = fixture.daemon.spaces.lock().unwrap();
    let space = spaces.iter().find(|space| space.name == name).unwrap();
    SpaceSelector::id(&space.id)
}

/// The variables the restores of these tests start with.
fn herdr() -> BTreeMap<String, String> {
    BTreeMap::from([("HERDR_ENV".into(), "1".into())])
}

/// Save the record of an agent stopped as `execution` in the space `id`.
fn stop(fixture: &Fixture, id: &SpaceId, execution: &str, record: &str) {
    let path = record_path(&fixture.agents.paths, id, &execution.into());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, record).unwrap();
}

const STOPPED: &str = r#"{"agent":"claude","mode":"worker","stage":"stopped"}"#;

/// Save the record and restore request a signal stop leaves.
fn signal_stop(fixture: &Fixture, id: &SpaceId, execution: &str) {
    stop(fixture, id, execution, STOPPED);
    let request = record_path(&fixture.agents.paths, id, &execution.into())
        .with_file_name(format!("{execution}.restore-request.json"));
    std::fs::write(request, r#"{"HERDR_ENV":"1"}"#).unwrap();
}

/// The variables the restore requested for `execution` starts with.
fn requested(
    fixture: &Fixture,
    workspace: &Workspace,
    execution: &str,
) -> Option<BTreeMap<String, String>> {
    recovery::restore_requests(&fixture.agents.paths, &workspace.id)
        .unwrap()
        .into_iter()
        .find_map(|(id, env)| (id == execution).then_some(env))
}

#[tokio::test]
async fn resumed_agents_start_from_the_daemon_as_slots_free() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 2);
    let mut workspaces = Vec::new();
    for name in ["one", "two", "three"] {
        let workspace = fixture.workspace(name).await;
        stop(&fixture, &workspace.id, &format!("{name}-agent"), STOPPED);
        workspaces.push(workspace);
    }
    // Three stopped agents exceed the limit, but a queued restore holds no
    // slot until it is admitted.
    let mut entries = Vec::new();
    for workspace in &workspaces {
        let execution = ExecutionId::from(format!("{}-agent", workspace.name));
        let entry = fixture
            .agents
            .queue_stopped_restore(
                &*fixture.daemon,
                &SpaceSelector::id(&workspace.id),
                &execution,
                herdr(),
            )
            .await
            .unwrap();
        assert_eq!(
            entry.execution.as_ref().map(ExecutionId::as_str),
            Some(execution.as_str())
        );
        entries.push(entry);
    }
    // Queueing again keeps the restore's place.
    let again = fixture
        .agents
        .queue_stopped_restore(
            &*fixture.daemon,
            &named(&fixture, "one"),
            &ExecutionId::from("one-agent"),
            herdr(),
        )
        .await
        .unwrap();
    assert_eq!(again.id, entries[0].id);

    dispatch(&fixture).await;
    assert!(started(&entries[0].id) && started(&entries[1].id));
    assert!(!started(&entries[2].id));
    assert_eq!(
        queue(&fixture).await,
        [
            ("one".into(), Restore, Starting),
            ("two".into(), Restore, Starting),
            ("three".into(), Restore, Queued),
        ]
    );

    // The first agent ran and ended, which frees its slot.
    fixture
        .agents
        .consume_queue_entry(&entries[0].id, &workspaces[0].id)
        .await;
    std::fs::remove_file(record_path(
        &fixture.agents.paths,
        &workspaces[0].id,
        &ExecutionId::from("one-agent"),
    ))
    .unwrap();
    dispatch(&fixture).await;
    assert!(started(&entries[2].id));
}

#[tokio::test]
async fn a_restore_started_now_starts_beyond_the_limit() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("work").await;
    // A launch takes the only slot before the agent stops.
    assert!(admitted(&add(&fixture, "busy").await));
    stop(&fixture, &workspace.id, "agent", STOPPED);
    let now = fixture
        .agents
        .start_stopped_restore(
            &*fixture.daemon,
            &named(&fixture, "work"),
            &ExecutionId::from("agent"),
            herdr(),
        )
        .await
        .unwrap();
    assert_eq!(now.state, Starting);
    assert!(started(&now.id));
}

#[tokio::test]
async fn only_an_ended_execution_with_a_record_can_be_queued() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("work").await;
    fixture
        .agents
        .queue_stopped_restore(
            &*fixture.daemon,
            &named(&fixture, "work"),
            &ExecutionId::from("absent"),
            herdr(),
        )
        .await
        .unwrap_err();
    let (running, _connection) = connected_agent(&fixture, &workspace).await;
    stop(&fixture, &workspace.id, running.as_str(), STOPPED);
    fixture
        .agents
        .queue_stopped_restore(
            &*fixture.daemon,
            &named(&fixture, "work"),
            &running,
            herdr(),
        )
        .await
        .unwrap_err();
}

/// The record of an agent stopped between turns.
const IDLE: &str = r#"{"agent":"claude","mode":"worker","stage":"stopped","turn":"idle"}"#;

/// The queued entry restoring `execution`.
async fn entry(fixture: &Fixture, execution: &str) -> QueueEntry {
    fixture
        .agents
        .queue_entries(&*fixture.daemon)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.execution.as_ref().map(ExecutionId::as_str) == Some(execution))
        .unwrap()
}

/// The record of an agent load stopped mid-turn.
const LOAD_STOPPED: &str = r#"{"agent":"claude","mode":"worker","stage":"stopped",
    "stop_reason":"critical memory pressure","for_load":true}"#;

/// Leave the restores of agents load stopped mid-turn in each of
/// `workspaces`, as their sessions' end requested them, to the daemon.
fn load_stops(fixture: &Fixture, workspaces: &[&Workspace]) {
    for workspace in workspaces {
        let execution = format!("{}-agent", workspace.name);
        signal_stop(fixture, &workspace.id, &execution);
        stop(fixture, &workspace.id, &execution, LOAD_STOPPED);
    }
}

/// A restore the daemon starts of an agent load stopped mid-turn waits for
/// healthy readings, one restore per healthy interval, as a waiting
/// wrapper's does.
#[tokio::test]
async fn a_daemon_started_restore_after_a_load_stop_waits_for_healthy_readings() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 4);
    let first = fixture.workspace("first").await;
    let second = fixture.workspace("second").await;
    load_stops(&fixture, &[&first, &second]);

    // Free slots alone do not restore them.
    dispatch(&fixture).await;
    assert_eq!(
        queue(&fixture).await,
        [
            ("first".into(), Restore, Queued),
            ("second".into(), Restore, Queued)
        ]
    );
    // Pressure that returns withdraws the readings before the pass.
    publish_recovery(&fixture);
    fixture.agents.overload_recovery().reset();
    dispatch(&fixture).await;
    assert!(!started(&entry(&fixture, "first-agent").await.id));

    // Each healthy interval restores one of them.
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    assert!(started(&entry(&fixture, "first-agent").await.id));
    assert_eq!(entry(&fixture, "second-agent").await.state, Queued);
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    assert!(started(&entry(&fixture, "second-agent").await.id));
}

/// A stop, reload or unsafe reading after healthy readings admitted a
/// restore the daemon starts returns it to its place as its agent takes
/// over, and the next healthy interval admits it again.
#[tokio::test]
async fn readings_withdrawn_before_a_daemon_started_load_restore_runs_return_it() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 4);
    let loaded = fixture.workspace("loaded").await;
    load_stops(&fixture, &[&loaded]);
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    let admitted = entry(&fixture, "loaded-agent").await;
    assert_eq!(admitted.state, Starting);

    fixture.agents.overload_recovery().reset();
    let refused = fixture.agents.reserve_queue_entry(&admitted.id).await;
    assert!(refused.is_err());
    assert_eq!(entry(&fixture, "loaded-agent").await.state, Queued);

    publish_recovery(&fixture);
    dispatch(&fixture).await;
    let readmitted = entry(&fixture, "loaded-agent").await;
    assert_eq!(readmitted.state, Starting);
    assert!(
        fixture
            .agents
            .reserve_queue_entry(&readmitted.id)
            .await
            .is_ok()
    );
}

/// A daemon restart returns a restore the daemon starts of the agent
/// `record` saves, which the previous daemon's healthy readings admitted, to
/// its place, where it waits for the new daemon's.
async fn a_daemon_restart_returns_an_admitted_restore_to_its_place(record: &str) {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 4);
    let loaded = fixture.workspace("loaded").await;
    signal_stop(&fixture, &loaded.id, "loaded-agent");
    stop(&fixture, &loaded.id, "loaded-agent", record);
    // Queueing an OOM restore counts its kill, which withdraws the readings
    // taken before it.
    dispatch(&fixture).await;
    publish_recovery(&fixture);
    dispatch(&fixture).await;
    let admitted = entry(&fixture, "loaded-agent").await;
    assert_eq!(admitted.state, Starting);

    let restarted = fixture.restart().await;
    limit_globally(&restarted, 4);
    restarted.agents.return_recovery_starts().await.unwrap();

    assert_eq!(entry(&restarted, "loaded-agent").await.state, Queued);
    assert!(
        restarted
            .agents
            .reserve_queue_entry(&admitted.id)
            .await
            .is_err()
    );
    dispatch(&restarted).await;
    assert_eq!(entry(&restarted, "loaded-agent").await.state, Queued);
    publish_recovery(&restarted);
    dispatch(&restarted).await;
    assert_eq!(entry(&restarted, "loaded-agent").await.state, Starting);
}

#[tokio::test]
async fn a_daemon_restart_returns_an_admitted_load_restore_to_its_place() {
    a_daemon_restart_returns_an_admitted_restore_to_its_place(LOAD_STOPPED).await;
}

#[tokio::test]
async fn a_daemon_restart_returns_an_admitted_oom_restore_to_its_place() {
    let killed =
        r#"{"agent":"claude","mode":"worker","stage":"stopped","cause":{"kind":"oom_kill"}}"#;
    a_daemon_restart_returns_an_admitted_restore_to_its_place(killed).await;
}

/// Disabling recovery leaves a restore the daemon would start after a load
/// stop for `shoal resume`, keeping the agent's record.
#[tokio::test]
async fn disabling_recovery_leaves_a_daemon_started_load_restore_for_shoal_restore() {
    let fixture = Fixture::new().await;
    let loaded = fixture.workspace("loaded").await;
    let idle = fixture.workspace("idle").await;
    load_stops(&fixture, &[&loaded]);
    signal_stop(&fixture, &idle.id, "idle-agent");
    stop(&fixture, &idle.id, "idle-agent", IDLE);
    fixture.configure(
        "[machine]\nmax_heavy_agents = 4\n[machine.overload.disk]\nenabled = false\n\
         [machine.overload.recovery]\nenabled = false",
    );

    dispatch(&fixture).await;

    // An idle stop's restore does not follow the recovery policy.
    assert_eq!(queue(&fixture).await, [("idle".into(), Restore, Queued)]);
    let record = record_path(&fixture.agents.paths, &loaded.id, &"loaded-agent".into());
    assert!(record.exists());
    let blocked = told(&fixture, AlertKind::RecoveryBlocked);
    assert!(
        (blocked.iter()).any(|message| message.contains(loaded.id.as_str())),
        "{blocked:?}"
    );
}

/// A chat's agent stopped between turns waits for an update before its
/// restore, as a workspace's does.
#[tokio::test]
async fn a_signal_stopped_idle_chat_agent_waits_for_an_update() {
    let fixture = Fixture::new().await;
    let repo = fixture.repository().await;
    let chat = fixture.chat_in("talk", "chat", Some(&repo)).await;
    signal_stop(&fixture, &chat, "idle-agent");
    stop(&fixture, &chat, "idle-agent", IDLE);

    dispatch(&fixture).await;

    let waiting = entry(&fixture, "idle-agent").await;
    assert!(waiting.waits_for_update && waiting.state == Queued);
}

#[tokio::test]
async fn resuming_an_agent_its_wrapper_waits_for_admits_the_wrappers_restore() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let mut workspaces = Vec::new();
    for name in ["idle", "resumed", "now"] {
        workspaces.push(fixture.workspace(name).await);
    }
    let agents = stopped_agents(&fixture, &workspaces.iter().collect::<Vec<_>>()).await;
    let mut restoring = Vec::new();
    for (workspace, (execution, _)) in workspaces.iter().zip(&agents) {
        stop(&fixture, &workspace.id, execution.as_str(), IDLE);
        restoring.push(await_restore(&fixture, execution).await);
    }
    dispatch(&fixture).await;
    let waiting = entry(&fixture, agents[1].0.as_str()).await;
    assert!(waiting.waits_for_update && waiting.state == Queued);

    // The wrapper's restore keeps its place and waits only for a slot.
    let resumed = fixture
        .agents
        .queue_stopped_restore(
            &*fixture.daemon,
            &named(&fixture, "resumed"),
            &agents[1].0,
            herdr(),
        )
        .await
        .unwrap();
    assert_eq!(resumed.id, waiting.id);
    assert!(!entry(&fixture, agents[1].0.as_str()).await.waits_for_update);
    dispatch(&fixture).await;
    let mut restoring = restoring.into_iter();
    let idle = restoring.next().unwrap();
    assert_eq!(
        bounded(restoring.next().unwrap()).await.unwrap().unwrap(),
        Grant::Queue
    );
    assert!(entry(&fixture, agents[0].0.as_str()).await.waits_for_update);

    // Started now, it goes beyond the limit in its wrapper.
    let now = fixture
        .agents
        .start_stopped_restore(
            &*fixture.daemon,
            &named(&fixture, "now"),
            &agents[2].0,
            herdr(),
        )
        .await
        .unwrap();
    assert_eq!(now.state, Starting);
    assert_eq!(
        bounded(restoring.next().unwrap()).await.unwrap().unwrap(),
        Grant::Queue
    );
    assert!(!started(&now.id) && !idle.is_finished());
    assert_eq!(
        fixture
            .agents
            .queue_entries(&*fixture.daemon,)
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn restores_leave_the_queue_once_resolved_or_cancelled() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let busy = fixture.workspace("busy").await;
    let (_running, _connection) = connected_agent(&fixture, &busy).await;
    let workspace = fixture.workspace("work").await;
    stop(&fixture, &workspace.id, "resumed", STOPPED);
    stop(&fixture, &workspace.id, "kept", STOPPED);
    for execution in ["resumed", "kept"] {
        fixture
            .agents
            .queue_stopped_restore(
                &*fixture.daemon,
                &named(&fixture, "work"),
                &execution.into(),
                herdr(),
            )
            .await
            .unwrap();
    }
    // Resuming the agent another way consumes its record.
    std::fs::remove_file(record_path(
        &fixture.agents.paths,
        &workspace.id,
        &ExecutionId::from("resumed"),
    ))
    .unwrap();
    dispatch(&fixture).await;
    assert_eq!(queue(&fixture).await, [("work".into(), Restore, Queued)]);

    // A stop cancels the restore and leaves the agent stopped.
    fixture
        .agents
        .stop_space(
            &*fixture.daemon,
            &named(&fixture, "work"),
            StopRequest::manual(),
        )
        .await
        .unwrap();
    assert!(queue(&fixture).await.is_empty());
    assert!(
        record_path(
            &fixture.agents.paths,
            &workspace.id,
            &ExecutionId::from("kept")
        )
        .exists()
    );
}

#[tokio::test]
async fn a_restore_whose_agent_never_starts_expires_and_says_so() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("work").await;
    stop(&fixture, &workspace.id, "agent", STOPPED);
    let entry = fixture
        .agents
        .queue_stopped_restore(
            &*fixture.daemon,
            &named(&fixture, "work"),
            &ExecutionId::from("agent"),
            herdr(),
        )
        .await
        .unwrap();
    dispatch(&fixture).await;
    assert!(started(&entry.id));
    let past = crate::time::UnixTime::now()
        - super::super::dispatch::START_GRACE
        - std::time::Duration::from_secs(1);
    fixture
        .agents
        .store
        .run(move |db| {
            db.execute("UPDATE agent_queue SET seen_at=?1", [past])?;
            Ok(())
        })
        .await
        .unwrap();

    dispatch(&fixture).await;
    assert!(queue(&fixture).await.is_empty());
    assert!(!told(&fixture, AlertKind::QueueFailed).is_empty());
    // The agent stays stopped for `shoal resume`.
    assert!(
        record_path(
            &fixture.agents.paths,
            &workspace.id,
            &ExecutionId::from("agent")
        )
        .exists()
    );
}

#[tokio::test]
async fn a_manual_stop_keeps_stopped_agents_as_last_sessions() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("work").await;
    signal_stop(&fixture, &workspace.id, "signalled");
    dispatch(&fixture).await;
    assert_eq!(queue(&fixture).await.len(), 1);
    fixture
        .agents
        .stop_space_manually(
            &*fixture.daemon,
            &named(&fixture, "work"),
            false,
            QueuedLaunches::Cancel,
        )
        .await
        .unwrap();
    // Nothing restores it, and it holds no slot or cleanup back.
    assert!(queue(&fixture).await.is_empty());
    assert!(
        !record_path(
            &fixture.agents.paths,
            &workspace.id,
            &ExecutionId::from("signalled")
        )
        .exists()
    );
    assert!(!recovery::pending(&fixture.agents.paths, &workspace.id).unwrap());
    let [last] = recovery::LastSession::all(&fixture.agents.paths, &workspace.id)
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(
        (last.execution.as_str(), last.agent.name()),
        ("signalled", "claude")
    );
}

#[tokio::test]
async fn a_last_session_queues_and_takes_a_slot_once_admitted() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let mut entries = Vec::new();
    for name in ["one", "two"] {
        let workspace = fixture.workspace(name).await;
        let execution = ExecutionId::from(format!("{name}-agent"));
        recovery::LastSession {
            narrowing: Default::default(),
            mode: "worker".into(),
            model: Default::default(),
            turn: None,
            execution: execution.clone(),
            agent: "claude".into(),
            session: None,
            moved: false,
            copied: None,
            origin: None,
            stop: None,
            converted: false,
            handoff: None,
        }
        .write(&fixture.agents.paths, &workspace.id)
        .unwrap();
        let entry = fixture
            .agents
            .queue_stopped_restore(
                &*fixture.daemon,
                &named(&fixture, name),
                &execution,
                herdr(),
            )
            .await
            .unwrap();
        entries.push(entry);
    }
    // The admitted restore counts as a launch until its agent registers.
    dispatch(&fixture).await;
    dispatch(&fixture).await;
    assert!(started(&entries[0].id));
    assert!(!started(&entries[1].id));
}

#[tokio::test]
async fn a_stop_cancels_an_admitted_restore_before_its_agent_registers() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("work").await;
    stop(&fixture, &workspace.id, "agent", STOPPED);
    let entry = fixture
        .agents
        .queue_stopped_restore(
            &*fixture.daemon,
            &named(&fixture, "work"),
            &ExecutionId::from("agent"),
            herdr(),
        )
        .await
        .unwrap();
    dispatch(&fixture).await;
    assert!(
        fixture
            .agents
            .queue_entry_starting(&entry.id)
            .await
            .unwrap()
    );
    // The agent may take over the slot only while its start is admitted.
    drop(fixture.agents.reserve_queue_entry(&entry.id).await.unwrap());
    fixture
        .agents
        .stop_space(
            &*fixture.daemon,
            &named(&fixture, "work"),
            StopRequest::manual(),
        )
        .await
        .unwrap();
    assert!(queue(&fixture).await.is_empty());
    fixture
        .agents
        .reserve_queue_entry(&entry.id)
        .await
        .unwrap_err();
}

#[tokio::test]
async fn a_shutdown_returns_an_admitted_restore_to_the_queue_when_its_agent_registers() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let workspace = fixture.workspace("work").await;
    stop(&fixture, &workspace.id, "agent", STOPPED);
    let entry = fixture
        .agents
        .queue_stopped_restore(
            &*fixture.daemon,
            &named(&fixture, "work"),
            &ExecutionId::from("agent"),
            herdr(),
        )
        .await
        .unwrap();
    dispatch(&fixture).await;
    assert!(
        fixture
            .agents
            .queue_entry_starting(&entry.id)
            .await
            .unwrap()
    );
    // The shutdown begins after admission, before the agent registers.
    fixture.agents.agent_queue().suspend(true).await;
    fixture
        .agents
        .reserve_queue_entry(&entry.id)
        .await
        .unwrap_err();
    dispatch(&fixture).await;
    assert_eq!(queue(&fixture).await[0].2, Queued);
    // Once the queue admits again, the restore starts again.
    fixture.agents.agent_queue().suspend(false).await;
    dispatch(&fixture).await;
    drop(fixture.agents.reserve_queue_entry(&entry.id).await.unwrap());
}

#[tokio::test]
async fn a_stop_forgets_a_request_while_a_resume_holds_its_record() {
    let fixture = Fixture::new().await;
    fixture
        .configure("[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false");
    let workspace = fixture.workspace("work").await;
    signal_stop(&fixture, &workspace.id, "agent");
    let path = record_path(
        &fixture.agents.paths,
        &workspace.id,
        &ExecutionId::from("agent"),
    );
    let resuming = crate::execution::recovery::claim(&path).unwrap();
    fixture
        .agents
        .stop_space(
            &*fixture.daemon,
            &named(&fixture, "work"),
            StopRequest::manual(),
        )
        .await
        .unwrap();
    // The resume fails and keeps the record, yet nothing restores the agent.
    drop(resuming);
    dispatch(&fixture).await;
    assert!(queue(&fixture).await.is_empty());
    assert!(path.exists());
}

#[tokio::test]
async fn a_request_whose_record_is_gone_is_forgotten() {
    let fixture = Fixture::new().await;
    fixture
        .configure("[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false");
    let workspace = fixture.workspace("work").await;
    signal_stop(&fixture, &workspace.id, "agent");
    // A resume consumed the record before dispatch saw the request.
    std::fs::remove_file(record_path(
        &fixture.agents.paths,
        &workspace.id,
        &ExecutionId::from("agent"),
    ))
    .unwrap();
    dispatch(&fixture).await;
    assert!(queue(&fixture).await.is_empty());
    assert_eq!(requested(&fixture, &workspace, "agent"), None);
}

#[tokio::test]
async fn stop_all_selects_running_agents_and_pending_restores() {
    let fixture = Fixture::new().await;
    limit_globally(&fixture, 1);
    let running = fixture.workspace("running").await;
    let (_execution, _connection) = connected_agent(&fixture, &running).await;
    let queued = fixture.workspace("queued").await;
    stop(&fixture, &queued.id, "waiting", STOPPED);
    fixture
        .agents
        .queue_stopped_restore(
            &*fixture.daemon,
            &named(&fixture, "queued"),
            &ExecutionId::from("waiting"),
            herdr(),
        )
        .await
        .unwrap();
    let requested = fixture.workspace("requested").await;
    signal_stop(&fixture, &requested.id, "agent");
    let idle = fixture.workspace("idle").await;
    stop(&fixture, &idle.id, "manual", STOPPED);

    let mut targets: Vec<String> = fixture
        .agents
        .stop_targets(&*fixture.daemon, QueuedLaunches::Cancel)
        .await
        .unwrap()
        .into_iter()
        .map(|space| space.name.clone().into())
        .collect();
    targets.sort();
    assert_eq!(targets, ["queued", "requested", "running"]);
}
