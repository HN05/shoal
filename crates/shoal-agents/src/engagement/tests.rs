use super::*;
use shoal_daemon_base::callers::Access;
use shoal_daemon_base::callers::Caller;

use crate::stand_in::Fixture;

/// A workspace's ID, with the execution its agent runs in.
async fn workspace(fixture: &Fixture) -> (SpaceId, ExecutionId) {
    let workspace = fixture.workspace("agent").await;
    let execution = fixture.execution(&workspace, "agent", None).await;
    (workspace.id, execution)
}

fn at(seconds: i64) -> UnixTime {
    UnixTime::from_seconds(seconds)
}

/// End a turn of the agent of `id` at `seconds`.
async fn end_turn(fixture: &Fixture, id: &SpaceId, seconds: i64) {
    let id = id.to_owned();
    fixture
        .agents
        .store
        .write(move |tx| {
            let working = Some(AgentState::Working);
            follow_state(tx, &id, working, AgentState::Idle, at(seconds))
        })
        .await
        .unwrap();
}

async fn interact_at(fixture: &Fixture, id: &SpaceId, seconds: i64) {
    fixture
        .agents
        .record_interaction(id, at(seconds))
        .await
        .unwrap();
}

async fn engagement(fixture: &Fixture, id: &SpaceId) -> Engagement {
    let id = id.to_owned();
    fixture
        .agents
        .store
        .read(move |db| get(db, &id))
        .await
        .unwrap()
}

async fn replies(fixture: &Fixture) -> Vec<i64> {
    fixture
        .agents
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
    let fixture = Fixture::new().await;
    let (id, _) = workspace(&fixture).await;
    end_turn(&fixture, &id, 100).await;
    assert!(!engagement(&fixture, &id).await.engaged());

    // The user's prompt starts the next turn.
    interact_at(&fixture, &id, 110).await;
    end_turn(&fixture, &id, 200).await;
    assert!(engagement(&fixture, &id).await.engaged());

    // An update starts a turn the user does not interact with.
    end_turn(&fixture, &id, 300).await;
    assert!(!engagement(&fixture, &id).await.engaged());

    interact_at(&fixture, &id, 310).await;
    assert!(engagement(&fixture, &id).await.engaged());
}

#[tokio::test]
async fn only_the_first_interaction_after_an_engaged_turn_gives_a_reply_time() {
    let fixture = Fixture::new().await;
    let (id, _) = workspace(&fixture).await;
    interact_at(&fixture, &id, 10).await;
    end_turn(&fixture, &id, 100).await;
    interact_at(&fixture, &id, 130).await;
    interact_at(&fixture, &id, 140).await;
    assert_eq!(replies(&fixture).await, [30]);

    // A turn the user was not engaged with gives none.
    end_turn(&fixture, &id, 200).await;
    end_turn(&fixture, &id, 300).await;
    interact_at(&fixture, &id, 400).await;
    assert_eq!(replies(&fixture).await, [30]);
}

#[tokio::test]
async fn a_reported_turn_end_counts_and_a_session_start_does_not() {
    let fixture = Fixture::new().await;
    let (id, execution) = workspace(&fixture).await;
    let space = SpaceSelector::id(&id);
    let report = async |state| {
        (fixture.agents.set_agent_state(
            &*fixture.daemon,
            &space,
            state,
            Some(execution.clone()),
            None,
        ))
        .await
        .unwrap();
    };
    report(AgentState::Idle).await;
    assert_eq!(engagement(&fixture, &id).await.turn_ended_at, None);
    report(AgentState::Working).await;
    report(AgentState::Idle).await;
    assert!(engagement(&fixture, &id).await.turn_ended_at.is_some());
}

#[tokio::test]
async fn a_coordinators_request_is_not_the_users_interaction() {
    let fixture = Fixture::new().await;
    let (id, _) = workspace(&fixture).await;
    let coordinator = Caller {
        space_id: "parent".into(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let space = SpaceSelector::id(&id);
    (fixture
        .agents
        .record_user_interaction(&*fixture.daemon, Some(&coordinator), &space))
    .await;
    assert_eq!(engagement(&fixture, &id).await.interacted_at, None);

    fixture
        .agents
        .record_user_interaction(&*fixture.daemon, None, &space)
        .await;
    assert!(engagement(&fixture, &id).await.interacted_at.is_some());
}

#[tokio::test]
async fn a_key_on_the_sleep_screen_counts_for_the_agents_space() {
    let fixture = Fixture::new().await;
    let (id, execution) = workspace(&fixture).await;
    fixture.agents.record_agent_interaction(&execution).await;
    assert!(engagement(&fixture, &id).await.interacted_at.is_some());
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
    let fixture = Fixture::new().await;
    let (id, _) = workspace(&fixture).await;
    fixture
        .agents
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
        (fixture.agents.store)
            .read(move |db| reply_time(db, &space, cap))
            .await
            .unwrap()
    };
    assert_eq!(reply(&id, 600).await, Some(Duration::from_secs(60)));
    assert_eq!(reply(&id, 40).await, Some(Duration::from_secs(35)));
}

#[tokio::test]
async fn a_message_the_user_sends_every_agent_counts_for_each_it_reached() {
    use shoal_protocol::alerts::{Reached, Recipient};
    let fixture = Fixture::new().await;
    let (id, _) = workspace(&fixture).await;
    let mut reached = Reached::default();
    reached.on_start.workspaces.push(Recipient {
        number: 1.into(),
        id: id.clone(),
        name: "agent".into(),
    });
    let coordinator = Caller::of_space("parent".into());
    fixture
        .agents
        .record_users_message(&*fixture.daemon, Some(&coordinator), &reached)
        .await;
    assert_eq!(engagement(&fixture, &id).await.interacted_at, None);

    fixture
        .agents
        .record_users_message(&*fixture.daemon, None, &reached)
        .await;
    assert!(engagement(&fixture, &id).await.interacted_at.is_some());
}
