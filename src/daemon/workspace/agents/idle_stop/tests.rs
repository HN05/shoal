use super::*;
use crate::{
    daemon::{
        notifications::NotificationKind,
        workspace::{ExecutionKind, StartedExecution},
    },
    execution::recovery::Record,
    model::Workspace,
    test_support::{manager, repository},
};

/// How a test workspace keeps its agent.
#[derive(Clone, Copy)]
struct Setup {
    state: AgentState,
    done: bool,
    held: bool,
    recover: bool,
}

const HELD_DONE_IDLE: Setup = Setup {
    state: AgentState::Idle,
    done: true,
    held: true,
    recover: true,
};

/// A workspace named `name` whose agent `setup` describes.
async fn agent(
    manager: &Manager,
    repository_id: &str,
    name: &str,
    setup: Setup,
) -> (Workspace, StartedExecution) {
    let workspace = manager
        .create_workspace(repository_id, name.into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    manager
        .track_agent(
            &execution.plan.id,
            "claude",
            &workspace.id,
            &workspace.name,
            setup.recover,
            std::time::Duration::ZERO,
        )
        .await;
    manager
        .set_agent_state(
            &workspace.name,
            setup.state,
            Some(execution.plan.id.clone()),
            None,
        )
        .await
        .unwrap();
    if setup.held {
        manager
            .acquire_hold(&workspace.id, "app".into(), None)
            .await
            .unwrap();
    }
    if setup.done {
        manager
            .mark_done(&workspace.id, true, false, Vec::new())
            .await
            .unwrap();
    }
    (workspace, execution)
}

/// Set when the workspace's prompt cache expires, `offset` seconds from now.
async fn cache_expires_in(manager: &Manager, workspace: &Workspace, offset: i64) {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspace_agent_state
                 SET prompt_cache_expires_at=unixepoch()+?2 WHERE workspace_id=?1",
                rusqlite::params![id, offset],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_held_done_workspaces_idle_agent_stops_once_its_cache_expires() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", HELD_DONE_IDLE).await;
    cache_expires_in(&manager, &workspace, 600).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(!*execution.stop.borrow(), "a warm cache keeps the agent");

    cache_expires_in(&manager, &workspace, 0).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(*execution.stop.borrow());
    let path =
        crate::execution::recovery::record_path(&manager.paths, &workspace.id, &execution.plan.id);
    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(record.turn, Some(AgentState::Idle));
    let notifications = manager.notifications(false, 10).await.unwrap();
    let stopped: Vec<_> = notifications
        .iter()
        .filter(|event| event.kind == NotificationKind::AgentStopped)
        .collect();
    assert_eq!(stopped.len(), 1);
    assert!(
        stopped[0].message.contains(
            "idle in a done workspace whose cleanup is held; \
             automatic session restore waits until an update arrives for it"
        ),
        "{}",
        stopped[0].message
    );
}

#[tokio::test]
async fn other_agents_keep_running() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut executions = Vec::new();
    for (name, setup) in [
        (
            "working",
            Setup {
                state: AgentState::Working,
                ..HELD_DONE_IDLE
            },
        ),
        (
            "unheld",
            Setup {
                held: false,
                ..HELD_DONE_IDLE
            },
        ),
        (
            "unfinished",
            Setup {
                done: false,
                ..HELD_DONE_IDLE
            },
        ),
        (
            "unrestorable",
            Setup {
                recover: false,
                ..HELD_DONE_IDLE
            },
        ),
    ] {
        executions.push((name, agent(&manager, &repo.id, name, setup).await.1));
    }
    manager.stop_idle_agents().await.unwrap();
    for (name, execution) in executions {
        assert!(!*execution.stop.borrow(), "{name} keeps running");
    }
}

#[tokio::test]
async fn idle_stop_stops_any_agent_once_its_cache_expires() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let unfinished = Setup {
        done: false,
        held: false,
        ..HELD_DONE_IDLE
    };
    let (workspace, execution) = agent(&manager, &repo.id, "idle", unfinished).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(!*execution.stop.borrow(), "idle_stop is opt-in");

    manager
        .write_local_config(&repo.id, Some("[idle_stop]\nenabled = true\n".into()))
        .await
        .unwrap();
    cache_expires_in(&manager, &workspace, 60).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(!*execution.stop.borrow(), "a warm cache keeps the agent");

    cache_expires_in(&manager, &workspace, 0).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(*execution.stop.borrow());
    let notifications = manager.notifications(false, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|event| event.kind == NotificationKind::AgentStopped
                && event.message.contains("idle with its prompt cache expired")),
        "{notifications:?}"
    );
}

#[tokio::test]
async fn a_cache_renewed_after_the_agent_was_chosen_keeps_it() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", HELD_DONE_IDLE).await;
    let chosen = chosen_held(&execution.plan.id, 0);
    // A turn ended after the sweep chose the agent.
    cache_expires_in(&manager, &workspace, 600).await;
    let turns = manager.agent_turns().await.unwrap();
    manager.stop_idle(turns, chosen, unix_now()).await;
    assert!(!*execution.stop.borrow());
}

#[tokio::test]
async fn a_turn_during_the_final_read_of_the_turns_keeps_the_agent() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", HELD_DONE_IDLE).await;
    let id = execution.plan.id.clone();
    let chosen = chosen_held(&id, 0);
    let turns = manager.agent_turns().await.unwrap();
    // A whole turn passes after the turns were read, so they show the agent
    // idle with the cache it had before.
    for state in [AgentState::Working, AgentState::Idle] {
        manager
            .set_agent_state(&workspace.name, state, Some(id.clone()), None)
            .await
            .unwrap();
    }
    manager.stop_idle(turns, chosen, unix_now()).await;
    assert!(!*execution.stop.borrow());
}

#[tokio::test]
async fn a_wake_after_an_idle_stop_leaves_its_update_for_the_restore() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", HELD_DONE_IDLE).await;
    let id = execution.plan.id.clone();
    manager.stop_idle_agents().await.unwrap();
    assert!(*execution.stop.borrow());
    let woken = manager
        .start_woken_turn(&workspace.name, Some(id))
        .await
        .unwrap();
    assert!(!woken, "the stop came first");
}

#[tokio::test]
async fn an_idle_stop_after_a_wake_keeps_the_agent() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (_, execution) = agent(&manager, &repo.id, "held", HELD_DONE_IDLE).await;
    let id = execution.plan.id.clone();
    let chosen = chosen_held(&id, 0);
    let turns = manager.agent_turns().await.unwrap();
    // The wake claims the turn before it records it.
    assert!(manager.claim_woken_turn(&id).await);
    manager.stop_idle(turns, chosen, unix_now()).await;
    assert!(!*execution.stop.borrow());
}

/// The sweep's choice to stop `execution` as idle in a held done workspace
/// when it had started `turns_started` turns.
fn chosen_held(execution: &str, turns_started: u64) -> HashMap<String, Chosen> {
    let stop = Chosen {
        protection: Protection::idle_held(),
        turns_started,
    };
    HashMap::from([(execution.to_owned(), stop)])
}

fn unix_now() -> i64 {
    i64::try_from(crate::time::unix_seconds()).unwrap()
}
