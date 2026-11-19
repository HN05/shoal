//! Daemon interruptions of stopped agents: a restart quarantines their
//! executions, and startup reconciles them from their recovery records.
use super::*;
use crate::state::ExecutionState;
use shoal_core::execution::recovery::Stage;
use shoal_daemon::test_daemon::overload::{blocked, executions, record, restart, store_reaches};
use shoal_protocol::space_names::SpaceSelector;

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
    assert!(shoal_core::execution::recovery::pending(&reopened.paths, &workspace.id).unwrap());
    assert!(!shoal_core::execution::recovery::any_pending(
        &reopened.paths
    ));
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
        let protection = shoal_protocol::model::Protection {
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
    shoal_core::execution::recovery::save_record(
        &manager.paths,
        &workspace.id,
        &id,
        &shoal_protocol::executions::TrackedAgent::named("fixture"),
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
    bounded(reopened.stop_space(&SpaceSelector::id(&workspace.id), StopRequest::manual()))
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
