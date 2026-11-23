//! The leases asleep agents keep, and those Shoal takes back from them, as
//! the daemon finds its asleep agents, their ranks and their settings.
use std::time::Duration;

use super::acquired::{Fixture, POOL, permit};
use crate::time::UnixTime;
use shoal_protocol::alerts::AlertKind;

/// Put an agent holding a permit and a simulator to sleep for `asleep`
/// under the global config `sleep`, then run the sweep's stop of long
/// sleepers' running leases, returning the fixture.
async fn sleep_with_leases(sleep: &str, asleep: Duration) -> Fixture {
    let fixture = Fixture::new(&format!("{POOL}[sleep]\n{sleep}\n")).await;
    let (execution, caller) = fixture.agent().await;
    fixture.acquire("permit", Some(&caller)).await;
    fixture.lease_simulator("sim", fixture.its_agent()).await;
    let since = UnixTime::now() - asleep;
    fixture.sleep_since(&execution, since).await;
    let manager = &fixture.manager;
    (manager.leases.stop_leases_of_long_sleepers(&**manager))
        .await
        .unwrap();
    fixture
}

/// The asleep agent's unread messages about leases taken back, and whether
/// any would wake it.
async fn taken(fixture: &Fixture) -> (Vec<String>, bool) {
    let unread = fixture
        .manager
        .agents
        .unread_messages(&fixture.workspace.id);
    let unread = unread.await.unwrap();
    let taken = (unread.alerts.into_iter())
        .filter(|alert| alert.kind == AlertKind::LeaseTaken)
        .map(|alert| alert.message)
        .collect();
    (taken, unread.wakes)
}

/// An agent asleep past `[sleep] keep_leases_minutes` loses its simulator
/// and keeps its permit, and reads why once it runs again, which does not
/// wake it.
#[tokio::test]
async fn an_agent_asleep_past_its_limit_loses_its_simulator_and_keeps_its_permit() {
    let fixture = sleep_with_leases("keep_leases_minutes = 30", Duration::from_secs(31 * 60)).await;

    assert_eq!(fixture.simulators().await, Vec::<String>::new());
    let agent = fixture.its_agent();
    assert_eq!(fixture.permits().await, [permit("permit", &agent)]);
    let (taken, wakes) = taken(&fixture).await;
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert!(taken[0].contains("after 30 minutes asleep"), "{}", taken[0]);
    assert!(!wakes);
}

/// `keep_leases_minutes = 0` stops an agent's running leases as it sleeps.
#[tokio::test]
async fn no_minutes_kept_stops_the_running_leases_as_the_agent_sleeps() {
    let fixture = sleep_with_leases("keep_leases_minutes = 0", Duration::ZERO).await;

    assert_eq!(fixture.simulators().await, Vec::<String>::new());
}

/// An agent asleep within its limit keeps its simulator.
#[tokio::test]
async fn an_agent_asleep_within_its_limit_keeps_its_simulator() {
    let fixture = sleep_with_leases("keep_leases_minutes = 30", Duration::from_secs(60)).await;

    assert_eq!(fixture.simulators().await, ["sim"]);
    assert_eq!(taken(&fixture).await.0, Vec::<String>::new());
}

/// Put an agent of the fixture's workspace, at `priority`, to sleep holding
/// the only permit of the pool `lock`, then request one for another
/// workspace, from that workspace's own process or the user's terminal,
/// returning the fixture and whether the request was granted.
async fn busy_request(priority: u8, from_space: bool) -> (Fixture, bool) {
    let fixture = Fixture::new("[resources.lock]\ncapacity = 1\n").await;
    let (execution, caller) = fixture.agent().await;
    fixture.acquire("asleep", Some(&caller)).await;
    fixture.sleep(&execution).await;
    let manager = &fixture.manager;
    let id = fixture.workspace.id.clone();
    let prioritized = manager.store.run(move |db| {
        db.execute(
            "INSERT INTO space_priority(space_id,priority) VALUES (?1,?2)",
            rusqlite::params![id, priority],
        )?;
        Ok(())
    });
    prioritized.await.unwrap();
    let repository = fixture.workspace.repository().clone();
    let other = manager.create_workspace(&repository, "other".into(), None, None, None);
    let other = other.await.unwrap();
    let request = shoal_protocol::resources::ResourceRequest {
        pool: "lock".into(),
        name: "mine".into(),
        ..Default::default()
    };
    let caller = from_space.then(|| crate::daemon::scope::Caller::of_space(other.id.clone()));
    let selector = shoal_protocol::space_names::SpaceSelector::id(&other.id);
    let acquired =
        (manager.leases).acquire_resource(&**manager, &selector, request, caller.as_ref());
    let granted = matches!(
        acquired.await.unwrap(),
        crate::daemon::allocation::Allocation::Granted(_)
    );
    (fixture, granted)
}

/// A busy request takes the permit of an asleep agent whose space's
/// priority is no higher than its own, which reads where it went.
#[tokio::test]
async fn a_busy_request_takes_an_asleep_agents_permit_of_no_higher_priority() {
    let (fixture, granted) = busy_request(50, true).await;

    assert!(granted);
    assert_eq!(fixture.permits().await, []);
    let (taken, _) = taken(&fixture).await;
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert!(taken[0].contains("given to space 2"), "{}", taken[0]);
}

/// An asleep agent of a higher priority keeps its permit from a space's
/// request.
#[tokio::test]
async fn an_asleep_agent_of_higher_priority_keeps_its_permit() {
    let (fixture, granted) = busy_request(100, true).await;

    assert!(!granted);
    let agent = fixture.its_agent();
    assert_eq!(fixture.permits().await, [permit("asleep", &agent)]);
}

/// The user's own request takes from any asleep agent.
#[tokio::test]
async fn the_users_request_takes_from_any_asleep_agent() {
    let (fixture, granted) = busy_request(100, false).await;

    assert!(granted);
    assert_eq!(fixture.permits().await, []);
}

/// An asleep agent keeps its permit with no end and its simulator until it
/// has slept its limit; an agent that is not asleep keeps nothing to show.
#[tokio::test]
async fn an_asleep_agent_keeps_its_permit_and_its_simulator_until_its_limit() {
    use shoal_protocol::model::{KeptKind, KeptLease};
    let fixture = Fixture::new(&format!("{POOL}[sleep]\nkeep_leases_minutes = 30\n")).await;
    let (execution, caller) = fixture.agent().await;
    fixture.acquire("permit", Some(&caller)).await;
    fixture.lease_simulator("sim", fixture.its_agent()).await;
    let (manager, id) = (&fixture.manager, &fixture.workspace.id);
    let kept = || manager.leases.kept_by(&**manager, id);
    assert_eq!(kept().await.unwrap(), []);

    let since = UnixTime::from_seconds(UnixTime::now().seconds());
    fixture.sleep_since(&execution, since).await;

    let mut keeps = kept().await.unwrap();
    keeps.sort_by_key(|kept| kept.kind.to_string());
    let lease = |kind, name: &str, until| KeptLease {
        kind,
        name: Some(name.into()),
        until,
    };
    let limit = since + Duration::from_secs(30 * 60);
    assert_eq!(
        keeps,
        [
            lease(KeptKind::Resource, "lock", None),
            lease(KeptKind::Simulator, "sim", Some(limit)),
        ]
    );
}
