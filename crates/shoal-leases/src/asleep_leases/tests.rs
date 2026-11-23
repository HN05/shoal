use std::time::Duration;

use super::*;
use crate::callers::Caller;
use crate::resources::ResourceRequest;
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
