use super::*;
use crate::daemon::{
    executions::ExecutionKind,
    manager::Manager,
    scope::Caller,
    test_support::{manager, repository},
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
        vouched: None,
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

const MAX_WAIT: Duration = Duration::from_secs(30 * 60);

#[test]
fn an_agent_the_user_is_not_engaged_with_waits_the_floor() {
    let away = Engagement {
        interacted_at: Some(at(50)),
        turn_ended_at: Some(at(1000)),
        previous_turn_ended_at: Some(at(100)),
    };
    let reply = Some(Duration::from_secs(600));
    assert_eq!(away.wait_ends(at(1000), reply, MAX_WAIT), at(1000) + FLOOR);
}

#[test]
fn an_engaged_user_gets_twice_their_reply_time_from_their_last_interaction() {
    let engaged = Engagement {
        interacted_at: Some(at(1100)),
        turn_ended_at: Some(at(1000)),
        previous_turn_ended_at: Some(at(500)),
    };
    let wait = |reply: Option<u64>| {
        let reply = reply.map(Duration::from_secs);
        engaged.wait_ends(at(1000), reply, MAX_WAIT).seconds()
    };
    assert_eq!(wait(Some(60)), 1100 + 120);
    assert_eq!(wait(Some(1)), 1100 + 10, "the floor");
    assert_eq!(wait(Some(3600)), 1100 + 1800, "the most it waits");
    assert_eq!(wait(None), 1100 + 1800, "without a reply time");
}

#[tokio::test]
async fn a_space_without_reply_times_takes_the_users_capped_average() {
    let (root, manager) = manager().await;
    let (id, _) = workspace(&manager, root.path()).await;
    manager
        .store
        .write(|tx| {
            for seconds in [30, 90] {
                tx.execute(
                    "INSERT INTO reply_times(space_id,seconds) VALUES ('other',?1)",
                    [seconds],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let reply = async |space: &SpaceId, cap: u64| {
        let (space, cap) = (space.to_owned(), Duration::from_secs(cap));
        (manager.store)
            .read(move |db| reply_time(db, &space, cap))
            .await
            .unwrap()
    };
    assert_eq!(reply(&id, 600).await, Some(Duration::from_secs(60)));
    assert_eq!(reply(&id, 40).await, Some(Duration::from_secs(35)));
}

#[tokio::test]
async fn a_message_the_user_sends_every_agent_counts_for_each_it_reached() {
    use shoal_protocol::agent_messages::{Reached, Recipient};
    let (root, manager) = manager().await;
    let (id, _) = workspace(&manager, root.path()).await;
    let mut reached = Reached::default();
    reached.on_start.workspaces.push(Recipient {
        number: 1.into(),
        id: id.clone(),
        name: "agent".into(),
    });
    let coordinator = Caller::of_space("parent".into());
    manager
        .record_users_message(Some(&coordinator), &reached)
        .await;
    assert_eq!(engagement(&manager, &id).await.interacted_at, None);

    manager.record_users_message(None, &reached).await;
    assert!(engagement(&manager, &id).await.interacted_at.is_some());
}
