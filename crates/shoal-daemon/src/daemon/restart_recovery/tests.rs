//! Daemon interruptions of stopped agents: a restart quarantines their
//! executions, and startup reconciles them from their recovery records.
use crate::daemon::test_support::overload::{
    blocked, executions, fixture, record, restart, store_reaches,
};
use crate::daemon::{manager::Manager, store};
use crate::execution::recovery::LastSessionExt;
use crate::process::identity::IdentityExt;
use crate::{
    execution::recovery::{Stage, record_path},
    model::Workspace,
    process::identity::{self as process, Identity},
    state::{AgentState, ExecutionState},
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use std::{fs, time::Duration};

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
            .reconcile_interrupted_agents()
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
    // A pass interrupted after keeping its session, which is gone.
    interrupted(&manager, &workspace, "kept", &earlier, None, None).await;
    let kept_session = LastSession {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        turn: None,
        execution: "kept".into(),
        agent: "fixture".into(),
        session: Some("kept-session".into()),
        moved: false,
        copied: None,
        converted: false,
        origin: None,
        stop: None,
    };
    kept_session.write(&manager.paths, &workspace.id).unwrap();
    // A recorded child keeps an execution of this boot that saved nothing.
    let wrapper = exited().await;
    interrupted(
        &manager,
        &workspace,
        "this-boot",
        &wrapper,
        Some(&wrapper),
        None,
    )
    .await;
    for id in ["unrecorded", "kept", "this-boot"] {
        fs::remove_file(record_path(&manager.paths, &workspace.id, &id.into())).unwrap();
    }
    manager
        .store
        .run(|db| {
            db.execute(
                "UPDATE executions SET agent='fixture' WHERE id IN ('unrecorded','kept')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    save_session(
        &manager.paths,
        &workspace.id,
        &ExecutionId::from("unrecorded"),
        &"rebooted-session".into(),
    )
    .unwrap();
    // The reboot ended its agent mid-turn, which its last session keeps.
    let unrecorded = Some(ExecutionId::from("unrecorded"));
    let selector = SpaceSelector::id(&workspace.id);
    let working = manager.set_agent_state(&selector, AgentState::Working, unrecorded, None);
    working.await.unwrap();
    let reopened = restart(&manager).await;
    reopened.reconcile_interrupted_agents().await.unwrap();
    let kept: Vec<_> = executions(&reopened, &workspace)
        .await
        .into_iter()
        .map(|execution| execution.id)
        .collect();
    assert_eq!(kept, ["this-boot"]);
    assert_eq!(
        record(&reopened, &workspace, &ExecutionId::from("rebooted"))
            .unwrap()
            .stop_reason
            .as_deref(),
        Some("test memory pressure")
    );
    let last = LastSession::all(&reopened.paths, &workspace.id).unwrap();
    assert_eq!(
        last,
        [
            kept_session,
            LastSession {
                narrowing: Default::default(),
                mode: "worker".into(),
                model: Default::default(),
                turn: Some(AgentState::Working),
                execution: "unrecorded".into(),
                agent: "fixture".into(),
                session: Some("rebooted-session".into()),
                moved: false,
                copied: None,
                converted: false,
                origin: None,
                stop: None,
            }
        ]
    );
    assert!(blocked(&reopened).await.is_empty());
    reopened.store.shutdown().await;
    serving.abort();
}

/// Record an execution whose wrapper never started its command: unknown,
/// with no child and no recovery record.
async fn unstarted(manager: &Manager, workspace: &Workspace, id: &str, wrapper: &Identity) {
    interrupted(manager, workspace, id, wrapper, None, None).await;
    fs::remove_file(record_path(&manager.paths, &workspace.id, &id.into())).unwrap();
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

#[tokio::test]
async fn reconciliation_forgets_an_unstarted_execution_whose_wrapper_exited() {
    let (_root, manager, workspace, serving) = fixture().await;
    let wrapper = exited().await;
    unstarted(&manager, &workspace, "unstarted", &wrapper).await;
    // A process group shows that its command started, though its child
    // exited before its identity was read.
    unstarted(&manager, &workspace, "started", &wrapper).await;
    manager
        .store
        .run(|db| {
            db.execute("UPDATE executions SET group_id=1 WHERE id='started'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let pending = manager.reconcile_interrupted_agents().await.unwrap();
    assert!(pending.waiting.is_empty() && !pending.unresolved);
    let kept: Vec<_> = (executions(&manager, &workspace).await.into_iter())
        .map(|execution| execution.id)
        .collect();
    assert_eq!(kept, ["started"]);
    assert!(blocked(&manager).await.is_empty());
    manager.store.shutdown().await;
    serving.abort();
}

#[tokio::test]
async fn reconciliation_forgets_an_unstarted_execution_whose_wrapper_pid_was_reused() {
    let (_root, manager, workspace, serving) = fixture().await;
    let (_later, live) = sleeper();
    unstarted(&manager, &workspace, "reused", &born_earlier(&live)).await;
    let pending = manager.reconcile_interrupted_agents().await.unwrap();
    assert!(pending.waiting.is_empty() && !pending.unresolved);
    assert!(executions(&manager, &workspace).await.is_empty());
    manager.store.shutdown().await;
    serving.abort();
}

#[tokio::test]
async fn reconciliation_keeps_an_unstarted_execution_while_its_wrapper_runs() {
    let (_root, manager, workspace, serving) = fixture().await;
    let (_alive, wrapper) = sleeper();
    unstarted(&manager, &workspace, "alive", &wrapper).await;
    let pending = manager.reconcile_interrupted_agents().await.unwrap();
    assert_eq!(pending.waiting.len(), 1);
    let kept = executions(&manager, &workspace).await;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].state, ExecutionState::Unknown);
    manager.store.shutdown().await;
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
        async move { super::reconcile(&manager, Duration::from_millis(50)).await }
    });
    store_reaches(&manager, async || !blocked(&manager).await.is_empty()).await;
    assert_eq!(executions(&manager, &workspace).await.len(), 1);
    survivor.kill().await.unwrap();
    survivor.wait().await.unwrap();
    store_reaches(&manager, async || {
        executions(&manager, &workspace).await.is_empty()
    })
    .await;
    assert!(record(&manager, &workspace, &ExecutionId::from("survivor")).is_some());
    reconciling.abort();
    serving.abort();
}
