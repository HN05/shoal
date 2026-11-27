use super::*;
use crate::execution::recovery::{Record, RecordExt as _, record_path, save_record};
use crate::stand_in::Fixture;
use shoal_protocol::executions::TrackedAgent;
use shoal_protocol::space_names::SpaceSelector;

/// The record of `execution`, saved as its wrapper saves it, once an OOM
/// kill ended it.
async fn killed(fixture: &Fixture, space: &SpaceId, execution: &str) -> Record {
    let (paths, execution) = (&fixture.agents.paths, &ExecutionId::from(execution));
    let agent = TrackedAgent::named("claude");
    let reason = Some("the OOM killer ended it");
    save_record(paths, space, execution, &agent, reason, None).unwrap();
    fixture.agents.record_oom_kill(space, execution).await;
    Record::load(&record_path(paths, space, execution))
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn kills_in_a_row_hold_back_the_restore_until_a_turn_finishes() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("killed").await;
    let first = killed(&fixture, &workspace.id, "first").await;
    assert_eq!(first.oom_kills, 1);
    // One kill costs no time beyond memory recovering.
    assert_eq!(first.restores_after, None);
    let before = UnixTime::now();
    let second = killed(&fixture, &workspace.id, "second").await;
    assert_eq!(second.oom_kills, 2);
    let held = second.restores_after.unwrap();
    assert!(held >= before + FIRST_DELAY, "{held:?}");
    // A finished turn of the restored agent forgets them.
    let execution = fixture.execution(&workspace, "restored", None).await;
    let selector = SpaceSelector::id(&workspace.id);
    for state in [AgentState::Working, AgentState::Idle] {
        (fixture.agents)
            .set_agent_state(
                &*fixture.daemon,
                &selector,
                state,
                Some(execution.clone()),
                None,
            )
            .await
            .unwrap();
    }
    let after_turn = killed(&fixture, &workspace.id, "restored").await;
    assert_eq!(after_turn.oom_kills, 1);
    assert_eq!(after_turn.restores_after, None);
}

#[test]
fn the_delay_doubles_then_settles_at_its_longest() {
    assert_eq!(delay(1), None);
    let delays: Vec<_> = (2..=6).map(|kills| delay(kills).unwrap()).collect();
    assert!(
        delays.windows(2).all(|pair| pair[0] < pair[1]),
        "{delays:?}"
    );
    assert_eq!(delays[1], delays[0] * 2);
    assert_eq!(delay(u32::MAX), Some(LONGEST_DELAY));
}

#[tokio::test]
async fn a_kill_withdraws_the_healthy_readings_taken_before_it() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("killed").await;
    let recovery = fixture.agents.overload_recovery();
    recovery.publish(Some(recovery.epoch()));
    assert!(recovery.published().is_some());
    killed(&fixture, &workspace.id, "killed").await;
    assert_eq!(recovery.published(), None);
}
