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
    stage: Stage,
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
        Stage::Stopped,
    )
    .await;
    let reopened = restart(&manager).await;
    assert!(
        reopened
            .reconcile_interrupted_agents()
            .await
            .unwrap()
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
        Stage::Stopped,
    )
    .await;
    // Only the wrapper proves that a stop it never acknowledged took effect.
    interrupted(
        &manager,
        &workspace,
        "unproven",
        &wrapper,
        None,
        Stage::Stopping,
    )
    .await;
    let (_alive, live) = sleeper();
    interrupted(
        &manager,
        &workspace,
        "reattaching",
        &live,
        None,
        Stage::Stopped,
    )
    .await;
    let reopened = restart(&manager).await;
    for _ in 0..2 {
        let waiting = reopened.reconcile_interrupted_agents().await.unwrap();
        assert_eq!(waiting.len(), 1);
    }
    let mut kept: Vec<_> = executions(&reopened, &workspace)
        .await
        .into_iter()
        .map(|execution| execution.id)
        .collect();
    kept.sort();
    assert_eq!(kept, ["reattaching", "survivor", "unproven"]);
    let messages = blocked(&reopened).await;
    assert_eq!(messages.len(), 2, "{messages:?}");
    let survivor = messages.iter().find(|m| m.contains("survivor")).unwrap();
    assert!(
        survivor.contains(&format!(
            "owned processes still running (PID {})",
            child.pid
        )),
        "{survivor}"
    );
    assert!(survivor.contains("after test memory pressure"));
    assert!(survivor.contains("shoal doctor load"));
    assert!(survivor.contains("shoal resume load --execution survivor"));
    let unproven = messages.iter().find(|m| m.contains("unproven")).unwrap();
    assert!(
        unproven.contains("process identity is incomplete"),
        "{unproven}"
    );
    assert!(record(&reopened, &workspace, "survivor").is_some());
    reopened.store.shutdown().await;
    serving.abort();
}

/// Protection stops the agent and the daemon dies while its wrapper waits for
/// an automatic restore. The wrapper exits, and the next daemon reconciles.
async fn crash_while_protected(stop: impl AsyncFnOnce(&Manager)) {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    stop(&manager).await;
    wait_paused(&manager, &workspace).await;
    serving.abort();
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 1);
    let id = executions(&manager, &workspace).await[0].id.clone();
    let reopened = restart(&manager).await;
    assert!(
        reopened
            .reconcile_interrupted_agents()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(executions(&reopened, &workspace).await.is_empty());
    let saved = record(&reopened, &workspace, &id).unwrap();
    assert_eq!(saved.stage, Stage::Stopped);
    assert!(saved.stop_reason.is_some());
    assert!(crate::execution::recovery::pending(&reopened.paths, &workspace.id).unwrap());
    assert!(blocked(&reopened).await.is_empty());
    reopened.store.shutdown().await;
}

#[tokio::test]
async fn daemon_crash_under_memory_pressure_leaves_the_agent_resumable() {
    crash_while_protected(async |manager| {
        assert!(
            manager
                .stop_agent_for_overload("test memory pressure")
                .await
        );
    })
    .await;
}

#[tokio::test]
async fn daemon_crash_under_disk_pressure_leaves_the_agent_resumable() {
    crash_while_protected(async |manager| {
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
        reopened.reconcile_interrupted_agents().await.unwrap().len(),
        1
    );
    assert_eq!(
        record(&reopened, &workspace, &id).unwrap().stage,
        Stage::Stopping
    );
    let serving = serve_fixture(reopened.clone());
    bounded(async {
        while executions(&reopened, &workspace).await[0].state != ExecutionState::Running {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(record(&reopened, &workspace, &id).is_none());
    assert!(blocked(&reopened).await.is_empty());
    // A stop the agent receives saves the wrapper's record as usual.
    bounded(reopened.stop_workspace(&workspace.id, StopRequest::manual()))
        .await
        .unwrap();
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        record(&reopened, &workspace, &id).unwrap().stage,
        Stage::Stopped
    );
    reopened.store.shutdown().await;
    serving.abort();
}
