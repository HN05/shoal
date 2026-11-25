//! Daemon interruptions of stopped agents: a restart quarantines their
//! executions, and startup reconciles them from their recovery records.
use crate::daemon::test_support::overload::{blocked, executions, fixture, record, restart};
use crate::daemon::{manager::Manager, store};
use crate::{
    execution::recovery::{Stage, record_path},
    model::Workspace,
    process::identity::{self as process, Identity},
    state::ExecutionState,
};
use shoal_ids::execution_ids::ExecutionId;
use std::fs;

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
    let (id, workspace_id) = (ExecutionId::from(id), workspace.id.clone());
    let (wrapper, child) = (wrapper.clone(), child.cloned());
    let group = child.as_ref().map(|child| child.pid);
    let saved = (id.clone(), workspace_id.clone());
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state,wrapper,child,group_id)
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
        "mode": "worker",
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
            .agents
            .reconcile_interrupted_agents(&*reopened)
            .await
            .unwrap()
            .waiting
            .is_empty()
    );
    assert!(executions(&reopened, &workspace).await.is_empty());
    assert_eq!(
        record(&reopened, &workspace, &ExecutionId::from("paused"))
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
        record_path(
            &manager.paths,
            &workspace.id,
            &ExecutionId::from("unreadable"),
        ),
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
        let pending = reopened
            .agents
            .reconcile_interrupted_agents(&*reopened)
            .await
            .unwrap();
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
    // Each blocked execution is named, and a survivor by its process.
    for id in ["survivor", "unproven", "legacy", "unreadable"] {
        assert!(messages.iter().any(|m| m.contains(id)), "{messages:?}");
    }
    let survivor = messages.iter().find(|m| m.contains("survivor")).unwrap();
    assert!(survivor.contains(&child.pid.to_string()), "{survivor}");
    assert!(record(&reopened, &workspace, &ExecutionId::from("survivor")).is_some());
    reopened.store.shutdown().await;
    serving.abort();
}
