use std::time::Duration;

use super::*;
use crate::callers::Caller;
use crate::resources::{LockMode, ResourceRequest};
use crate::stand_in::Fixture;
use shoal_protocol::acquirers::Acquirer;

const POOL: &str = "[resources.lock]\ncapacity = 4\n";

/// The agent of `space`, asleep for `asleep` and keeping its running
/// leases for `keeps`.
fn sleeper(space: &SpaceId, asleep: Duration, keeps: Duration) -> Sleeper {
    Sleeper {
        space: space.clone(),
        since: UnixTime::now() - asleep,
        keeps_running: keeps,
    }
}

fn agent(space: &SpaceId) -> Acquirer {
    Acquirer::Agent {
        space_id: space.clone(),
    }
}

/// Acquire a permit of the pool `lock` named `name` as the agent of `space`.
async fn acquire_permit(fixture: &Fixture, name: &str, space: &SpaceId) {
    let request = ResourceRequest {
        mode: None,
        kind: None,
        pool: "lock".into(),
        name: name.into(),
        resource: None,
        reason: None,
        queue: false,
    };
    let (caller, selector) = (Caller::of_space(space.clone()), fixture.selector());
    let acquired =
        (fixture.leases).acquire_resource(&fixture.daemon, &selector, request, Some(&caller));
    assert!(matches!(
        acquired.await.unwrap(),
        crate::allocation::Allocation::Granted(_)
    ));
}

/// An agent asleep past its limit loses its simulator, keeps its permit and
/// reads why; one within its limit keeps its simulator.
#[tokio::test]
async fn an_agent_asleep_past_its_limit_loses_its_simulator_and_keeps_its_permit() {
    let fixture = Fixture::new(POOL, "").await;
    let [overslept, napping] = [(); 2].map(|()| SpaceId::generate());
    fixture.lease_simulator("long", agent(&overslept)).await;
    acquire_permit(&fixture, "kept", &overslept).await;
    fixture.lease_simulator("short", agent(&napping)).await;
    let minutes = |n: u64| Duration::from_secs(n * 60);
    *fixture.daemon.sleepers.lock().unwrap() = vec![
        sleeper(&overslept, minutes(31), minutes(30)),
        sleeper(&napping, minutes(29), minutes(30)),
    ];

    (fixture.leases)
        .stop_leases_of_long_sleepers(&fixture.daemon)
        .await
        .unwrap();

    assert_eq!(fixture.simulators().await, ["short"]);
    let permits = fixture.leases.list_resources(&fixture.daemon, None).await;
    assert_eq!(permits.unwrap().len(), 1);
    let told = fixture.daemon.told.lock().unwrap().clone();
    assert_eq!(told.len(), 1);
    let (space, message) = &told[0];
    assert_eq!(space, &overslept);
    assert!(message.contains("30 minutes"), "{message}");
    assert!(
        message.contains("`shoal acquire sim --name long --space 1`"),
        "{message}"
    );
}

/// Memory pressure stops one running lease, the lowest-ranked asleep
/// agent's, and nothing once no asleep agent holds one.
#[tokio::test]
async fn memory_pressure_stops_one_running_lease_of_the_lowest_ranked_sleeper() {
    let fixture = Fixture::new(POOL, "").await;
    let [lowest, higher] = [(); 2].map(|()| SpaceId::generate());
    fixture.lease_simulator("lowest", agent(&lowest)).await;
    fixture.lease_simulator("higher", agent(&higher)).await;
    let long = Duration::from_secs(3600);
    *fixture.daemon.sleepers.lock().unwrap() = vec![
        sleeper(&lowest, Duration::ZERO, long),
        sleeper(&higher, Duration::ZERO, long),
    ];
    let stop = || {
        fixture
            .leases
            .stop_a_sleepers_running_lease(&fixture.daemon)
    };

    assert!(stop().await.unwrap());
    assert_eq!(fixture.simulators().await, ["higher"]);
    assert!(stop().await.unwrap());
    assert!(!stop().await.unwrap());
    let told = fixture.daemon.told.lock().unwrap().clone();
    assert!(told[0].1.contains("to free memory"), "{}", told[0].1);
}

/// Request a lease of the pool `lock` named `name` in `mode`, as the user,
/// returning whether it was granted.
async fn request_lock(fixture: &Fixture, name: &str, mode: Option<LockMode>) -> bool {
    let request = ResourceRequest {
        mode,
        kind: None,
        pool: "lock".into(),
        name: name.into(),
        resource: None,
        reason: None,
        queue: false,
    };
    let selector = fixture.selector();
    let acquired = (fixture.leases).acquire_resource(&fixture.daemon, &selector, request, None);
    matches!(
        acquired.await.unwrap(),
        crate::allocation::Allocation::Granted(_)
    )
}

/// The names of the pool's leases, with who acquired each.
async fn holders(fixture: &Fixture) -> Vec<(String, Acquirer)> {
    let leases = fixture.leases.list_resources(&fixture.daemon, None).await;
    let mut holders: Vec<_> = (leases.unwrap().into_iter())
        .map(|lease| (lease.name, lease.acquirer))
        .collect();
    holders.sort_by(|a, b| a.0.cmp(&b.0));
    holders
}

/// Acquire a lease of the pool `lock` named `name` in `mode` as the agent
/// of `space`.
async fn agent_lock(fixture: &Fixture, name: &str, space: &SpaceId, mode: Option<LockMode>) {
    let request = ResourceRequest {
        mode,
        kind: None,
        pool: "lock".into(),
        name: name.into(),
        resource: None,
        reason: None,
        queue: false,
    };
    let (caller, selector) = (Caller::of_space(space.clone()), fixture.selector());
    let acquired =
        (fixture.leases).acquire_resource(&fixture.daemon, &selector, request, Some(&caller));
    assert!(matches!(
        acquired.await.unwrap(),
        crate::allocation::Allocation::Granted(_)
    ));
}

/// A busy request takes the permit of the lowest-ranked asleep agent, who
/// reads that it went to the request's space.
#[tokio::test]
async fn a_busy_request_takes_the_lowest_ranked_asleep_agents_permit() {
    let fixture = Fixture::new("[resources.lock]\ncapacity = 2\n", "").await;
    let [lowest, higher] = [(); 2].map(|()| SpaceId::generate());
    acquire_permit(&fixture, "higher", &higher).await;
    acquire_permit(&fixture, "lowest", &lowest).await;
    let long = Duration::from_secs(3600);
    *fixture.daemon.sleepers.lock().unwrap() = vec![
        sleeper(&lowest, Duration::ZERO, long),
        sleeper(&higher, Duration::ZERO, long),
    ];

    assert!(request_lock(&fixture, "mine", None).await);

    let higher = permit_of("higher", agent(&higher));
    assert_eq!(
        holders(&fixture).await,
        [higher, permit_of("mine", Acquirer::User)]
    );
    let told = fixture.daemon.told.lock().unwrap().clone();
    assert_eq!(told.len(), 1);
    assert_eq!(told[0].0, lowest);
    assert!(told[0].1.contains("given to space 1"), "{}", told[0].1);
}

/// A running agent's permit is never taken: the request stays busy.
#[tokio::test]
async fn a_busy_request_leaves_a_running_agents_permit() {
    let fixture = Fixture::new("[resources.lock]\ncapacity = 1\n", "").await;
    let running = SpaceId::generate();
    acquire_permit(&fixture, "held", &running).await;

    assert!(!request_lock(&fixture, "mine", None).await);
    assert_eq!(
        holders(&fixture).await,
        [permit_of("held", agent(&running))]
    );
}

/// A writer takes asleep readers' leases only when every reader in its way
/// is asleep.
#[tokio::test]
async fn a_writer_takes_readers_leases_only_when_every_reader_sleeps() {
    let pool = "[resources.lock]\nkind = \"rwlock\"\ncapacity = 1\n";
    let fixture = Fixture::new(pool, "").await;
    let [asleep, running] = [(); 2].map(|()| SpaceId::generate());
    let read = Some(LockMode::Read);
    agent_lock(&fixture, "asleep", &asleep, read).await;
    agent_lock(&fixture, "running", &running, read).await;
    let long = Duration::from_secs(3600);
    *fixture.daemon.sleepers.lock().unwrap() = vec![sleeper(&asleep, Duration::ZERO, long)];

    assert!(!request_lock(&fixture, "writer", Some(LockMode::Write)).await);
    assert_eq!(holders(&fixture).await.len(), 2);

    fixture
        .daemon
        .sleepers
        .lock()
        .unwrap()
        .push(sleeper(&running, Duration::ZERO, long));
    assert!(request_lock(&fixture, "writer", Some(LockMode::Write)).await);
    assert_eq!(
        holders(&fixture).await,
        [permit_of("writer", Acquirer::User)]
    );
}

fn permit_of(name: &str, acquirer: Acquirer) -> (String, Acquirer) {
    (name.to_owned(), acquirer)
}
