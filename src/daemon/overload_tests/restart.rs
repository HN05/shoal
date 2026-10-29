//! Daemon interruptions of stopped agents: a restart quarantines their
//! executions, and startup reconciles them from their recovery records.
use super::*;
use crate::{
    execution::recovery::{Record, Stage, record_path},
    model::{Execution, Workspace},
    process::identity::{self as process, Identity},
    state::ExecutionState,
};

/// Open the state as a restarted daemon does before it serves.
async fn restart(manager: &Manager) -> Arc<Manager> {
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    reopened
        .store
        .quarantine_interrupted_operations()
        .await
        .unwrap();
    reopened
}

async fn executions(manager: &Manager, workspace: &Workspace) -> Vec<Execution> {
    manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions
}

/// Wait until a committed state change satisfies `reached`.
async fn store_reaches(manager: &Manager, mut reached: impl AsyncFnMut() -> bool) {
    // Subscribe before the first check so a concurrent commit cannot be missed.
    let mut changed = manager.store.watch_changes();
    bounded(async {
        while !reached().await {
            changed.changed().await.unwrap();
        }
    })
    .await;
}

fn record(manager: &Manager, workspace: &Workspace, id: &str) -> Option<Record> {
    Record::load(&record_path(&manager.paths, &workspace.id, id)).unwrap()
}

async fn blocked(manager: &Manager) -> Vec<String> {
    manager
        .notifications(false, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|notification| notification.kind == NotificationKind::RecoveryBlocked)
        .map(|notification| notification.message)
        .collect()
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
    manager: &Manager,
    workspace: &Workspace,
    id: &str,
    wrapper: &Identity,
    child: Option<&Identity>,
    stage: Option<Stage>,
) {
    let (id, workspace_id) = (id.to_owned(), workspace.id.clone());
    let (wrapper, child) = (wrapper.clone(), child.cloned());
    let group = child.as_ref().map(|child| child.pid);
    let saved = (id.clone(), workspace_id.clone());
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,workspace_id,state,wrapper,child,group_id)
                    VALUES (?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    id,
                    workspace_id,
                    ExecutionState::Unknown,
                    store::json_text(Some(&wrapper))?,
                    store::json_text(child.as_ref())?,
                    group,
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (id, workspace_id) = saved;
    let saved = serde_json::json!({
        "agent": "fixture",
        "stop_reason": "test memory pressure",
        "stage": stage,
    });
    let path = record_path(&manager.paths, &workspace_id, &id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, saved.to_string()).unwrap();
}

#[tokio::test]
async fn restart_forgets_a_stopped_agent_whose_wrapper_exited_and_keeps_its_record() {
    let (_root, manager, workspace, serving) = fixture().await;
    let wrapper = exited().await;
    // A paused agent's daemon already forgot its stopped child.
    interrupted(
        &manager,
        &workspace,
        "paused",
        &wrapper,
        None,
        Some(Stage::Stopped),
    )
    .await;
    let reopened = restart(&manager).await;
    assert!(
        reopened
            .reconcile_interrupted_agents()
            .await
            .unwrap()
            .waiting
            .is_empty()
    );
    assert!(executions(&reopened, &workspace).await.is_empty());
    assert_eq!(
        record(&reopened, &workspace, "paused")
            .unwrap()
            .stop_reason
            .as_deref(),
        Some("test memory pressure")
    );
    assert!(blocked(&reopened).await.is_empty());
    reopened.store.shutdown().await;
    serving.abort();
}

/// Restarting the machine ended every process of an earlier boot, though
/// their recorded process group IDs may name processes of this one. An
/// agent that saved no record leaves its session for `shoal resume`.
#[tokio::test]
async fn reconciliation_forgets_executions_of_an_earlier_boot_keeping_what_they_saved() {
    use crate::execution::recovery::{LastSession, save_session};
    let (_root, manager, workspace, serving) = fixture().await;
    let (_alive, live) = sleeper();
    let earlier = live.recorded_in("an-earlier-boot");
    interrupted(
        &manager,
        &workspace,
        "rebooted",
        &earlier,
        Some(&earlier),
        Some(Stage::Stopping),
    )
    .await;
    interrupted(&manager, &workspace, "unrecorded", &earlier, None, None).await;
    let wrapper = exited().await;
    interrupted(&manager, &workspace, "this-boot", &wrapper, None, None).await;
    for id in ["unrecorded", "this-boot"] {
        fs::remove_file(record_path(&manager.paths, &workspace.id, id)).unwrap();
    }
    manager
        .store
        .run(|db| {
            db.execute(
                "UPDATE executions SET agent='fixture' WHERE id='unrecorded'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    save_session(
        &manager.paths,
        &workspace.id,
        "unrecorded",
        "rebooted-session",
    )
    .unwrap();
    let reopened = restart(&manager).await;
    reopened.reconcile_interrupted_agents().await.unwrap();
    let kept: Vec<_> = executions(&reopened, &workspace)
        .await
        .into_iter()
        .map(|execution| execution.id)
        .collect();
    assert_eq!(kept, ["this-boot"]);
    assert_eq!(
        record(&reopened, &workspace, "rebooted")
            .unwrap()
            .stop_reason
            .as_deref(),
        Some("test memory pressure")
    );
    let last = LastSession::all(&reopened.paths, &workspace.id).unwrap();
    assert_eq!(
        last,
        [LastSession {
            execution: "unrecorded".into(),
            agent: "fixture".into(),
            session: Some("rebooted-session".into()),
            moved: false,
            origin: None,
        }]
    );
    assert!(blocked(&reopened).await.is_empty());
    reopened.store.shutdown().await;
    serving.abort();
}

#[tokio::test]
async fn restart_keeps_uncertain_stopped_agents_and_notifies_once() {
    let (_root, manager, workspace, serving) = fixture().await;
    let wrapper = exited().await;
    let (_survivor, child) = sleeper();
    interrupted(
        &manager,
        &workspace,
        "survivor",
        &wrapper,
        Some(&child),
        Some(Stage::Stopped),
    )
    .await;
    // Only the wrapper proves that a stop it never acknowledged took effect.
    interrupted(
        &manager,
        &workspace,
        "unproven",
        &wrapper,
        None,
        Some(Stage::Stopping),
    )
    .await;
    interrupted(&manager, &workspace, "legacy", &wrapper, None, None).await;
    interrupted(&manager, &workspace, "unreadable", &wrapper, None, None).await;
    fs::write(
        record_path(&manager.paths, &workspace.id, "unreadable"),
        "not json",
    )
    .unwrap();
    let (_alive, live) = sleeper();
    interrupted(
        &manager,
        &workspace,
        "reattaching",
        &live,
        None,
        Some(Stage::Stopped),
    )
    .await;
    let reopened = restart(&manager).await;
    for _ in 0..2 {
        let pending = reopened.reconcile_interrupted_agents().await.unwrap();
        assert_eq!(pending.waiting.len(), 1);
        assert!(pending.unresolved);
    }
    let mut kept: Vec<_> = executions(&reopened, &workspace)
        .await
        .into_iter()
        .map(|execution| execution.id)
        .collect();
    kept.sort();
    assert_eq!(
        kept,
        [
            "legacy",
            "reattaching",
            "survivor",
            "unproven",
            "unreadable"
        ]
    );
    let messages = blocked(&reopened).await;
    assert_eq!(messages.len(), 4, "{messages:?}");
    let survivor = messages.iter().find(|m| m.contains("survivor")).unwrap();
    assert!(
        survivor.contains(&format!(
            "owned processes still running (PID {})",
            child.pid
        )),
        "{survivor}"
    );
    assert!(survivor.contains("after test memory pressure"));
    assert!(survivor.contains("shoal stop load"));
    assert!(survivor.contains("shoal resume load --execution survivor"));
    let unproven = messages.iter().find(|m| m.contains("unproven")).unwrap();
    assert!(
        unproven.contains("process identity is incomplete"),
        "{unproven}"
    );
    let legacy = messages.iter().find(|m| m.contains("legacy")).unwrap();
    assert!(
        legacy.contains("process identity is incomplete"),
        "{legacy}"
    );
    let unreadable = messages.iter().find(|m| m.contains("unreadable")).unwrap();
    assert!(unreadable.contains("cannot be read"), "{unreadable}");
    assert!(unreadable.contains("; check or remove "), "{unreadable}");
    assert!(record(&reopened, &workspace, "survivor").is_some());
    reopened.store.shutdown().await;
    serving.abort();
}

#[tokio::test]
async fn unresolved_stopped_agents_are_reconciled_once_their_survivors_exit() {
    let (_root, manager, workspace, serving) = fixture().await;
    let wrapper = exited().await;
    let (mut survivor, child) = sleeper();
    interrupted(
        &manager,
        &workspace,
        "survivor",
        &wrapper,
        Some(&child),
        Some(Stage::Stopped),
    )
    .await;
    let reconciling = tokio::spawn({
        let manager = manager.clone();
        async move {
            super::super::restart_recovery::reconcile(&manager, Duration::from_millis(50)).await
        }
    });
    store_reaches(&manager, async || !blocked(&manager).await.is_empty()).await;
    assert_eq!(executions(&manager, &workspace).await.len(), 1);
    survivor.kill().await.unwrap();
    survivor.wait().await.unwrap();
    store_reaches(&manager, async || {
        executions(&manager, &workspace).await.is_empty()
    })
    .await;
    assert!(record(&manager, &workspace, "survivor").is_some());
    reconciling.abort();
    serving.abort();
}

/// How the daemon goes away while a stopped agent waits for its restore.
enum Interruption {
    Crash,
    Shutdown,
}

/// Protection stops the agent and the daemon goes away while its wrapper waits
/// for an automatic restore. The wrapper reattaches to the next daemon, which
/// continues the wait and restores the agent once load recovers.
async fn interrupt_while_protected(interruption: Interruption, stop: impl AsyncFnOnce(&Manager)) {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    stop(&manager).await;
    wait_paused(&manager, &workspace).await;
    let id = executions(&manager, &workspace).await[0].id.clone();
    if let Interruption::Shutdown = interruption {
        // A daemon stops accepting before it shuts down.
        fs::remove_file(&manager.paths.socket).unwrap();
        bounded(manager.stop_for_shutdown()).await;
    }
    serving.abort();
    let reopened = restart(&manager).await;
    // The waiting wrapper restores the agent, so only cleanup counts it as
    // stopped work.
    let saved = record(&reopened, &workspace, &id).unwrap();
    assert_eq!(saved.stage, Some(Stage::Stopped));
    assert!(saved.waiting.is_some());
    assert!(crate::execution::recovery::pending(&reopened.paths, &workspace.id).unwrap());
    assert!(!crate::execution::recovery::any_pending(&reopened.paths));
    let serving = serve_fixture(reopened.clone());
    // Reattachment restores the protection stop after recording the execution
    // as running.
    bounded(async {
        while !reopened.agent_was_overloaded(&id).await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(
        executions(&reopened, &workspace).await[0].state,
        ExecutionState::Running
    );
    // An update waits for the restore here too.
    assert!(reopened.gates.try_quiesce().is_none());
    assert!(!launched.is_finished());
    assert!(!workspace.path.join("restored").exists());
    publish_recovery(&reopened);
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    assert!(record(&reopened, &workspace, &id).is_none());
    assert!(blocked(&reopened).await.is_empty());
    reopened.store.shutdown().await;
    serving.abort();
}

async fn stop_for_memory(manager: &Manager) {
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
}

#[tokio::test]
async fn daemon_crash_under_memory_pressure_keeps_the_agent_waiting_for_its_restore() {
    interrupt_while_protected(Interruption::Crash, stop_for_memory).await;
}

#[tokio::test]
async fn daemon_crash_under_disk_pressure_keeps_the_agent_waiting_for_its_restore() {
    interrupt_while_protected(Interruption::Crash, async |manager| {
        let protection = crate::daemon::workspace::Protection {
            reason: "test disk pressure".into(),
            resumes_when: "free disk space recovers".into(),
        };
        assert_eq!(
            manager.stop_agents_for_disk_space(&protection).await.len(),
            1
        );
    })
    .await;
}

#[tokio::test]
async fn daemon_shutdown_hands_an_agent_waiting_for_its_restore_to_the_next_daemon() {
    interrupt_while_protected(Interruption::Shutdown, stop_for_memory).await;
}

/// Protection saved its stop, then an update restarted the daemon before the
/// stop reached the wrapper, which detached with its agent still running.
#[tokio::test]
async fn update_restart_discards_a_stop_that_never_reached_the_agent() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let id = executions(&manager, &workspace).await[0].id.clone();
    crate::execution::recovery::save_record(
        &manager.paths,
        &workspace.id,
        &id,
        "fixture",
        Some("test memory pressure"),
        None,
    )
    .unwrap();
    // A daemon stops accepting before it shuts down; the next one serves the
    // reattachment.
    fs::remove_file(&manager.paths.socket).unwrap();
    bounded(manager.stop_for_shutdown()).await;
    serving.abort();
    let reopened = restart(&manager).await;
    // The live wrapper may still reattach, so nothing is decided yet.
    assert_eq!(
        reopened
            .reconcile_interrupted_agents()
            .await
            .unwrap()
            .waiting
            .len(),
        1
    );
    assert_eq!(
        record(&reopened, &workspace, &id).unwrap().stage,
        Some(Stage::Stopping)
    );
    let serving = serve_fixture(reopened.clone());
    // Reattachment commits `Running`, then discards the stop before it
    // releases the connection lock.
    store_reaches(&reopened, async || {
        executions(&reopened, &workspace).await[0].state == ExecutionState::Running
    })
    .await;
    drop(reopened.executions.connections().await);
    assert!(record(&reopened, &workspace, &id).is_none());
    assert!(blocked(&reopened).await.is_empty());
    // A stop the agent receives saves the wrapper's record as usual.
    bounded(reopened.stop_workspace(&workspace.id, StopRequest::manual()))
        .await
        .unwrap();
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        record(&reopened, &workspace, &id).unwrap().stage,
        Some(Stage::Stopped)
    );
    reopened.store.shutdown().await;
    serving.abort();
}
