use super::*;
use crate::daemon::{
    scope::Caller,
    test_support::{manager, repository},
    workspace::{ExecutionKind, Manager},
};

/// A workspace's ID, with the execution its agent runs in.
async fn workspace(manager: &Manager, root: &std::path::Path) -> (SpaceId, ExecutionId) {
    let repo = repository(root, "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    (workspace.id, execution.plan.id)
}

fn at(seconds: i64) -> UnixTime {
    UnixTime::from_seconds(seconds)
}

/// End a turn of the agent of `id` at `seconds`.
async fn end_turn(manager: &Manager, id: &SpaceId, seconds: i64) {
    let id = id.to_owned();
    manager
        .store
        .write(move |tx| {
            let working = Some(AgentState::Working);
            follow_state(tx, &id, working, AgentState::Idle, at(seconds))
        })
        .await
        .unwrap();
}

async fn interact_at(manager: &Manager, id: &SpaceId, seconds: i64) {
    manager.record_interaction(id, at(seconds)).await.unwrap();
}

async fn engagement(manager: &Manager, id: &SpaceId) -> Engagement {
    let id = id.to_owned();
    manager.store.read(move |db| get(db, &id)).await.unwrap()
}

async fn replies(manager: &Manager) -> Vec<i64> {
    manager
        .store
        .read(|db| {
            Ok(db
                .prepare("SELECT seconds FROM reply_times ORDER BY id")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn the_user_stays_engaged_until_a_turn_passes_without_them() {
    let (root, manager) = manager().await;
    let (id, _) = workspace(&manager, root.path()).await;
    end_turn(&manager, &id, 100).await;
    assert!(!engagement(&manager, &id).await.engaged());

    // The user's prompt starts the next turn.
    interact_at(&manager, &id, 110).await;
    end_turn(&manager, &id, 200).await;
    assert!(engagement(&manager, &id).await.engaged());

    // An update starts a turn the user does not interact with.
    end_turn(&manager, &id, 300).await;
    assert!(!engagement(&manager, &id).await.engaged());

    interact_at(&manager, &id, 310).await;
    assert!(engagement(&manager, &id).await.engaged());
}

#[tokio::test]
async fn only_the_first_interaction_after_an_engaged_turn_gives_a_reply_time() {
    let (root, manager) = manager().await;
    let (id, _) = workspace(&manager, root.path()).await;
    interact_at(&manager, &id, 10).await;
    end_turn(&manager, &id, 100).await;
    interact_at(&manager, &id, 130).await;
    interact_at(&manager, &id, 140).await;
    assert_eq!(replies(&manager).await, [30]);

    // A turn the user was not engaged with gives none.
    end_turn(&manager, &id, 200).await;
    end_turn(&manager, &id, 300).await;
    interact_at(&manager, &id, 400).await;
    assert_eq!(replies(&manager).await, [30]);
}

#[tokio::test]
async fn a_reported_turn_end_counts_and_a_session_start_does_not() {
    let (root, manager) = manager().await;
    let (id, execution) = workspace(&manager, root.path()).await;
    let space = SpaceSelector::id(&id);
    let report = async |state| {
        (manager.set_agent_state(&space, state, Some(execution.clone()), None))
            .await
            .unwrap();
    };
    report(AgentState::Idle).await;
    assert_eq!(engagement(&manager, &id).await.turn_ended_at, None);
    report(AgentState::Working).await;
    report(AgentState::Idle).await;
    assert!(engagement(&manager, &id).await.turn_ended_at.is_some());
}

#[tokio::test]
async fn a_coordinators_request_is_not_the_users_interaction() {
    let (root, manager) = manager().await;
    let (id, _) = workspace(&manager, root.path()).await;
    let coordinator = Caller {
        space_id: "parent".into(),
        execution: None,
        granted: None,
        read_only: false,
    };
    let space = SpaceSelector::id(&id);
    (manager.record_user_interaction(Some(&coordinator), &space)).await;
    assert_eq!(engagement(&manager, &id).await.interacted_at, None);

    manager.record_user_interaction(None, &space).await;
    assert!(engagement(&manager, &id).await.interacted_at.is_some());
}

#[tokio::test]
async fn a_key_on_the_sleep_screen_counts_for_the_agents_space() {
    let (root, manager) = manager().await;
    let (id, execution) = workspace(&manager, root.path()).await;
    manager.record_agent_interaction(&execution).await;
    assert!(engagement(&manager, &id).await.interacted_at.is_some());
}

#[tokio::test]
async fn a_prompt_the_user_submits_counts_as_interaction() {
    let (root, manager) = manager().await;
    let (id, execution) = workspace(&manager, root.path()).await;
    (manager.reply_in_chat(&SpaceSelector::id(&id), Some(&execution)))
        .await
        .unwrap();
    assert!(engagement(&manager, &id).await.interacted_at.is_some());
}
