//! Startup reconciles the unknown executions of stopped agents from their
//! recovery records and the processes they left.
use shoal_store::test_support::seeds;
use std::fs;

use super::*;
use crate::execution::recovery::{LastSession, LastSessionExt};
use crate::model::Workspace;
use crate::process::identity::{Identity, IdentityExt};
use crate::stand_in::Fixture;
use crate::state::AgentState;
use shoal_protocol::space_names::SpaceSelector;

/// The service over the stand-in with a workspace, holding the lock the
/// tests that read the whole process table share. Another test's
/// terminating process may have an unreadable environment before it
/// disappears, so they take turns through a file lock, which tests in
/// other processes see too; .config/nextest.toml groups them.
struct Locked {
    fixture: Fixture,
    workspace: Workspace,
    _lock: fs::File,
}

async fn locked() -> Locked {
    let lock = tokio::task::spawn_blocking(|| {
        let path = std::env::temp_dir().join("shoal-overload-tests.lock");
        let file = (fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true))
        .open(path)
        .unwrap();
        file.lock().unwrap();
        file
    });
    let lock = lock.await.unwrap();
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("load").await;
    Locked {
        fixture,
        workspace,
        _lock: lock,
    }
}

/// A process whose identity stays verifiable until it is killed.
fn sleeper() -> (tokio::process::Child, Identity) {
    let child = tokio::process::Command::new("sleep")
        .arg("60")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let identity = process::capture(child.id().unwrap()).unwrap().unwrap();
    (child, identity)
}

async fn exited() -> Identity {
    let (mut child, identity) = sleeper();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    identity
}

/// Record an execution as a restart leaves it, with the stop's handoff saved.
async fn interrupted(
    locked: &Locked,
    id: &str,
    wrapper: &Identity,
    child: Option<&Identity>,
    stage: Option<Stage>,
) {
    let (id, workspace_id) = (ExecutionId::from(id), locked.workspace.id.clone());
    let (wrapper, child) = (wrapper.clone(), child.cloned());
    let group = child.as_ref().map(|child| child.pid);
    let saved = (id.clone(), workspace_id.clone());
    (locked.fixture.agents.store)
        .run(move |db| {
            let mut recorded =
                shoal_protocol::test_support::execution(id.as_str(), workspace_id.as_str());
            recorded.state = ExecutionState::Unknown;
            (recorded.wrapper, recorded.child, recorded.group_id) = (Some(wrapper), child, group);
            seeds::execution(db, &recorded, None)
        })
        .await
        .unwrap();
    let (id, workspace_id) = saved;
    let saved = serde_json::json!({
        "agent": "fixture",
        "stop_reason": "test memory pressure",
        "stage": stage,
        "mode": "worker",
    });
    let path = record_path(&locked.fixture.agents.paths, &workspace_id, &id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, saved.to_string()).unwrap();
}

/// Record an execution whose wrapper never started its command: unknown,
/// with no child and no recovery record.
async fn unstarted(locked: &Locked, id: &str, wrapper: &Identity) {
    interrupted(locked, id, wrapper, None, None).await;
    let paths = &locked.fixture.agents.paths;
    fs::remove_file(record_path(paths, &locked.workspace.id, &id.into())).unwrap();
}

/// `identity` as a process born just before the one now at its PID.
fn born_earlier(identity: &Identity) -> Identity {
    let (start, tick) = identity.birth.rsplit_once(':').unwrap();
    let tick: u64 = tick.parse().unwrap();
    Identity {
        birth: format!("{start}:{}", tick - 1),
        ..identity.clone()
    }
}

/// The executions the store records for the workspace, by ID.
async fn kept(locked: &Locked) -> Vec<String> {
    let executions = locked.fixture.agents.space_executions(&locked.workspace.id);
    (executions.await.unwrap().into_iter())
        .map(|execution| execution.id.into())
        .collect()
}

/// The recovery record of the execution `id`.
fn record(locked: &Locked, id: &str) -> Option<Record> {
    let paths = &locked.fixture.agents.paths;
    Record::load(&record_path(paths, &locked.workspace.id, &id.into())).unwrap()
}

/// The messages of the alerts that a recovery is blocked.
fn blocked(fixture: &Fixture) -> Vec<String> {
    let notified = fixture.daemon.notified.lock().unwrap();
    (notified.iter())
        .filter(|(kind, _)| *kind == AlertKind::RecoveryBlocked)
        .map(|(_, message)| message.clone())
        .collect()
}

/// Reconcile once, as startup does.
async fn reconcile_once(fixture: &Fixture) -> Pending {
    (fixture
        .agents
        .reconcile_interrupted_agents(&*fixture.daemon))
    .await
    .unwrap()
}

/// Restarting the machine ended every process of an earlier boot, though
/// their recorded process group IDs may name processes of this one. Each
/// agent is saved for the agent queue, as a system shutdown saves it.
#[tokio::test]
async fn reconciliation_forgets_executions_of_an_earlier_boot_saving_their_agents_for_the_queue() {
    use crate::execution::recovery::{restore_requests, save_session};
    let locked = locked().await;
    let (paths, workspace) = (&locked.fixture.agents.paths, &locked.workspace);
    let (_alive, live) = sleeper();
    let earlier = live.recorded_in("an-earlier-boot");
    let stopping = Some(Stage::Stopping);
    interrupted(&locked, "rebooted", &earlier, Some(&earlier), stopping).await;
    // Claude Code resumes its latest session without a picker.
    let rebooted = record_path(paths, &workspace.id, &"rebooted".into());
    let saved = fs::read_to_string(&rebooted).unwrap();
    fs::write(&rebooted, saved.replace("\"fixture\"", "\"claude\"")).unwrap();
    interrupted(&locked, "unrecorded", &earlier, None, None).await;
    // A save the previous pass left after its restore request.
    interrupted(&locked, "half-saved", &earlier, None, None).await;
    // Its tool has no resume arguments, so its restore needs a picker.
    interrupted(&locked, "picked", &earlier, None, None).await;
    // A wrapper that finished saving chose its agent's restore itself.
    let stopped = Some(Stage::Stopped);
    interrupted(&locked, "stopped", &earlier, None, stopped).await;
    // A recorded child keeps an execution of this boot that saved nothing.
    let wrapper = exited().await;
    interrupted(&locked, "this-boot", &wrapper, Some(&wrapper), None).await;
    for id in ["unrecorded", "half-saved", "picked", "this-boot"] {
        fs::remove_file(record_path(paths, &workspace.id, &id.into())).unwrap();
    }
    (locked.fixture.agents.store)
        .run(|db| {
            db.execute(
                "UPDATE executions SET agent='claude' WHERE id IN ('unrecorded','half-saved')",
                [],
            )?;
            db.execute(
                "UPDATE executions SET agent='fixture' WHERE id='picked'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let env = std::collections::BTreeMap::new();
    crate::execution::recovery::request_restore(paths, &workspace.id, &"half-saved".into(), &env)
        .unwrap();
    let unrecorded = ExecutionId::from("unrecorded");
    save_session(
        paths,
        &workspace.id,
        &unrecorded,
        &"rebooted-session".into(),
    )
    .unwrap();
    // The reboot ended its agent mid-turn, which its record keeps.
    let selector = SpaceSelector::id(&workspace.id);
    let fixture = &locked.fixture;
    let working = AgentState::Working;
    (fixture.agents)
        .set_agent_state(&*fixture.daemon, &selector, working, Some(unrecorded), None)
        .await
        .unwrap();
    let Locked {
        fixture,
        workspace,
        _lock,
    } = locked;
    let locked = Locked {
        fixture: fixture.restart().await,
        workspace,
        _lock,
    };
    reconcile_once(&locked.fixture).await;
    assert_eq!(kept(&locked).await, ["this-boot"]);
    let rebooted = record(&locked, "rebooted").unwrap();
    assert_eq!(
        rebooted.stop_reason.as_deref(),
        Some("test memory pressure")
    );
    let saved = record(&locked, "unrecorded").unwrap();
    assert_eq!(saved.stop_reason.as_deref(), Some("its machine went down"));
    assert_eq!(saved.session, Some("rebooted-session".into()));
    assert_eq!(saved.turn, Some(AgentState::Working));
    assert_eq!(saved.stage, Some(Stage::Stopped));
    assert!(record(&locked, "picked").is_some());
    assert!(record(&locked, "half-saved").is_some());
    let paths = &locked.fixture.agents.paths;
    let requested: Vec<String> = (restore_requests(paths, &locked.workspace.id).unwrap())
        .into_iter()
        .map(|(id, _)| id.into())
        .collect();
    assert_eq!(requested, ["half-saved", "rebooted", "unrecorded"]);
    let id = locked.workspace.id.clone();
    let stopped = (locked.fixture.agents.store)
        .run(move |db| {
            let kinds = [shoal_daemon_base::events::EventKind::AgentStopped];
            shoal_daemon_base::events::newest_of(db, &id, &kinds)
        })
        .await
        .unwrap()
        .unwrap();
    let cause = stopped.event.agent().and_then(|agent| agent.cause);
    assert_eq!(
        cause,
        Some(shoal_protocol::stop_causes::StopCause::MachineDown)
    );
    assert!(
        LastSession::all(paths, &locked.workspace.id)
            .unwrap()
            .is_empty()
    );
    assert!(blocked(&locked.fixture).is_empty());
}

#[tokio::test]
async fn reconciliation_forgets_an_unstarted_execution_whose_wrapper_exited() {
    let locked = locked().await;
    let wrapper = exited().await;
    unstarted(&locked, "unstarted", &wrapper).await;
    // A process group shows that its command started, though its child
    // exited before its identity was read.
    unstarted(&locked, "started", &wrapper).await;
    (locked.fixture.agents.store)
        .run(|db| {
            db.execute("UPDATE executions SET group_id=1 WHERE id='started'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let pending = reconcile_once(&locked.fixture).await;
    assert!(pending.waiting.is_empty() && !pending.unresolved);
    assert_eq!(kept(&locked).await, ["started"]);
    assert!(blocked(&locked.fixture).is_empty());
}

#[tokio::test]
async fn reconciliation_forgets_an_unstarted_execution_whose_wrapper_pid_was_reused() {
    let locked = locked().await;
    let (_later, live) = sleeper();
    unstarted(&locked, "reused", &born_earlier(&live)).await;
    let pending = reconcile_once(&locked.fixture).await;
    assert!(pending.waiting.is_empty() && !pending.unresolved);
    assert!(kept(&locked).await.is_empty());
}

#[tokio::test]
async fn reconciliation_keeps_an_unstarted_execution_while_its_wrapper_runs() {
    let locked = locked().await;
    let (_alive, wrapper) = sleeper();
    unstarted(&locked, "alive", &wrapper).await;
    let pending = reconcile_once(&locked.fixture).await;
    assert_eq!(pending.waiting.len(), 1);
    let executions = locked.fixture.agents.space_executions(&locked.workspace.id);
    let executions = executions.await.unwrap();
    let [execution] = executions.as_slice() else {
        panic!("{executions:?}");
    };
    assert_eq!(execution.state, ExecutionState::Unknown);
}

#[tokio::test]
async fn unresolved_stopped_agents_are_reconciled_once_their_survivors_exit() {
    let locked = Arc::new(locked().await);
    let wrapper = exited().await;
    let (mut survivor, child) = sleeper();
    let stopped = Some(Stage::Stopped);
    interrupted(&locked, "survivor", &wrapper, Some(&child), stopped).await;
    let reconciling = tokio::spawn({
        let locked = locked.clone();
        async move {
            let fixture = &locked.fixture;
            reconcile(&fixture.agents, &*fixture.daemon, Duration::from_millis(50)).await
        }
    });
    let store = &locked.fixture.agents.store;
    // Subscribe before the first check so a concurrent commit cannot be missed.
    let mut changed = store.watch_changes();
    let notified = async {
        while blocked(&locked.fixture).is_empty() {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(60), notified)
        .await
        .expect("the survivor was never reported");
    assert_eq!(kept(&locked).await.len(), 1);
    survivor.kill().await.unwrap();
    survivor.wait().await.unwrap();
    let forgotten = async {
        while !kept(&locked).await.is_empty() {
            changed.changed().await.unwrap();
        }
    };
    tokio::time::timeout(Duration::from_secs(60), forgotten)
        .await
        .expect("the execution was never forgotten");
    assert!(record(&locked, "survivor").is_some());
    reconciling.abort();
}
