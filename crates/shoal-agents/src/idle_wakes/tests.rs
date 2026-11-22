use super::*;
use crate::{
    model::Workspace,
    stand_in::Fixture,
    state::{AgentState, Sleep},
};
use shoal_protocol::{config::modes::ModeName, space_names::SpaceSelector};

const HOUR: Duration = Duration::from_secs(3600);

#[test]
fn an_agent_is_due_each_period_it_stays_idle_counted_from_its_last_turn() {
    let id = SpaceId::generate();
    let since = UnixTime::from_seconds(1_000);
    let mut woken = Woken::default();
    assert!(!woken.due(&id, since, HOUR, since + HOUR - Duration::from_secs(1)));
    assert!(woken.due(&id, since, HOUR, since + HOUR));
    woken.0.insert(id.clone(), since + HOUR);
    // Without a turn, the next wake waits another period, without backoff.
    assert!(!woken.due(&id, since, HOUR, since + HOUR + HOUR / 2));
    assert!(woken.due(&id, since, HOUR, since + HOUR * 2));
    // A turn ending later counts from its end.
    let later = since + HOUR * 3;
    assert!(!woken.due(&id, later, HOUR, later + HOUR / 2));
    assert!(woken.due(&id, later, HOUR, later + HOUR));
}

/// A workspace for each of `names`, its agent recorded in the worker mode
/// and idle.
async fn idle_workers<const N: usize>(fixture: &Fixture, names: [&str; N]) -> [Workspace; N] {
    let mut created = Vec::new();
    for name in names {
        let workspace = fixture.workspace(name).await;
        fixture.update_space(&workspace.id, |space| space.mode = Some(ModeName::DEFAULT));
        report(fixture, &workspace.id, AgentState::Idle).await;
        created.push(workspace);
    }
    created.try_into().unwrap()
}

async fn report(fixture: &Fixture, id: &SpaceId, state: AgentState) {
    (fixture
        .agents
        .set_agent_state(&fixture.daemon, &SpaceSelector::id(id), state, None, None))
    .await
    .unwrap();
}

/// The unread messages of the space `id`.
async fn messages(fixture: &Fixture, id: &SpaceId) -> Vec<String> {
    (fixture
        .agents
        .agent_messages(&fixture.daemon, &SpaceSelector::id(id))
        .await
        .unwrap())
    .into_iter()
    .map(|message| message.message)
    .collect()
}

/// Run a sweep an hour and a minute from now.
async fn sweep_later(fixture: &Fixture, woken: &mut Woken) {
    let later = UnixTime::now() + HOUR + Duration::from_secs(60);
    fixture
        .agents
        .wake_idle_agents(&fixture.daemon, woken, later)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_worker_idle_for_its_period_is_woken_unless_done_or_sleeping() {
    let fixture = Fixture::new().await;
    let names = ["idle", "done", "sleeping", "permanent"];
    let [idle, done, sleeping, permanent] = idle_workers(&fixture, names).await;
    let selector = |workspace: &Workspace| SpaceSelector::id(&workspace.id);
    fixture.done(&done.id).await;
    let slept = fixture
        .agents
        .sleep(
            &fixture.daemon,
            &selector(&sleeping),
            Some(Sleep::Turn),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        (slept.sleep, slept.idle_wake_minutes),
        (Some(Sleep::Turn), Some(60))
    );
    (fixture.agents.sleep(
        &fixture.daemon,
        &selector(&permanent),
        Some(Sleep::Permanent),
        None,
    ))
    .await
    .unwrap();
    let mut woken = Woken::default();
    // Not yet idle for its period.
    fixture
        .agents
        .wake_idle_agents(&fixture.daemon, &mut woken, UnixTime::now())
        .await
        .unwrap();
    assert!(messages(&fixture, &idle.id).await.is_empty());
    sweep_later(&fixture, &mut woken).await;
    let wake = messages(&fixture, &idle.id).await;
    assert_eq!(wake.len(), 1);
    assert!(wake[0].contains("idle for 60 minutes"), "{wake:?}");
    for quiet in [&done, &sleeping, &permanent] {
        assert!(messages(&fixture, &quiet.id).await.is_empty());
    }
    // A turn ends a plain sleep, so the agent idle after it is woken again;
    // a permanent sleep lasts.
    for agent in [&sleeping, &permanent] {
        report(&fixture, &agent.id, AgentState::Working).await;
        report(&fixture, &agent.id, AgentState::Idle).await;
    }
    sweep_later(&fixture, &mut Woken::default()).await;
    assert_eq!(messages(&fixture, &sleeping.id).await.len(), 1);
    assert!(messages(&fixture, &permanent.id).await.is_empty());
    // Undo ends a permanent sleep.
    let undone = (fixture
        .agents
        .sleep(&fixture.daemon, &selector(&permanent), None, None))
    .await
    .unwrap();
    assert_eq!(undone.sleep, None);
    sweep_later(&fixture, &mut Woken::default()).await;
    assert_eq!(messages(&fixture, &permanent.id).await.len(), 1);
}

#[tokio::test]
async fn a_sleep_lasts_through_its_turn_and_a_wait_for_the_user() {
    let fixture = Fixture::new().await;
    let [workspace] = idle_workers(&fixture, ["waits"]).await;
    let selector = SpaceSelector::id(&workspace.id);
    report(&fixture, &workspace.id, AgentState::Working).await;
    fixture
        .agents
        .sleep(&fixture.daemon, &selector, Some(Sleep::Turn), None)
        .await
        .unwrap();
    // A permission prompt in the turn that slept starts no turn.
    report(&fixture, &workspace.id, AgentState::Waiting).await;
    report(&fixture, &workspace.id, AgentState::Working).await;
    report(&fixture, &workspace.id, AgentState::Idle).await;
    let id = workspace.id.clone();
    let recorded = (fixture.agents.store)
        .read(move |db| sleep::current(db, &id))
        .await
        .unwrap();
    assert_eq!(recorded, Some(Sleep::Turn));
}

#[tokio::test]
async fn a_wake_read_before_the_agent_worked_again_is_not_queued() {
    let fixture = Fixture::new().await;
    let [worked] = idle_workers(&fixture, ["worked"]).await;
    let mut idle = (fixture.agents.store.read(|db| turn_state::idle(db)))
        .await
        .unwrap();
    // Before the wake is queued, the agent works and goes idle again, within
    // the same second.
    report(&fixture, &worked.id, AgentState::Working).await;
    report(&fixture, &worked.id, AgentState::Idle).await;
    let later = UnixTime::now() + HOUR * 2;
    (fixture.agents.wake_if_due(
        &fixture.daemon,
        &idle.remove(0),
        &mut Woken::default(),
        later,
    ))
    .await
    .unwrap();
    assert!(messages(&fixture, &worked.id).await.is_empty());
}
