use super::*;
use crate::hooks::HookKind;
use crate::resources::ResourceRequest;
use crate::stand_in::{Fixture, StandInHook};

const POOL: &str = "[resources.lock]\ncapacity = 4\n";

/// A caller of the agent of `space`, acquiring for the fixture's workspace,
/// as a coordinator does for a space below.
fn agent(space: &SpaceId) -> Caller {
    let mut caller = Caller::of_space(space.clone());
    caller.execution = Some(crate::callers::ScopedExecution {
        id: ExecutionId::generate(),
        kind: shoal_protocol::executions::ExecutionKind::Command,
        agent_execution: None,
    });
    caller
}

/// Acquire a permit of the pool `lock` named `name` as `caller`.
async fn acquire(fixture: &Fixture, name: &str, caller: Option<&Caller>) {
    let request = ResourceRequest {
        mode: None,
        kind: None,
        pool: "lock".into(),
        name: name.into(),
        resource: None,
        reason: None,
        queue: false,
    };
    let (leases, selector) = (&fixture.leases, fixture.selector());
    let acquired = leases.acquire_resource(&fixture.daemon, &selector, request, caller);
    assert!(matches!(
        acquired.await.unwrap(),
        crate::allocation::Allocation::Granted(_)
    ));
}

/// The workspace's permits by name, with who acquired each.
async fn permits(fixture: &Fixture) -> Vec<(String, Acquirer)> {
    let leases = fixture.leases.list_resources(&fixture.daemon, None).await;
    let mut permits: Vec<_> = (leases.unwrap().into_iter())
        .map(|lease| (lease.name, lease.acquirer))
        .collect();
    permits.sort_by(|a, b| a.0.cmp(&b.0));
    permits
}

/// The sweep releases the permits of an agent whose space has no agent
/// left, keeps those of an agent that has not ended and the user's.
#[tokio::test]
async fn the_sweep_releases_the_permits_of_ended_agents_alone() {
    let fixture = Fixture::new(POOL, "").await;
    let [ended, live] = [(); 2].map(|()| SpaceId::generate());
    acquire(&fixture, "ended", Some(&agent(&ended))).await;
    acquire(&fixture, "live", Some(&agent(&live))).await;
    acquire(&fixture, "user", None).await;
    *fixture.daemon.agents.lock().unwrap() = [live.clone()].into();

    (fixture
        .leases
        .release_leases_of_ended_executions(&fixture.daemon))
    .await
    .unwrap();

    let live = Acquirer::Agent { space_id: live };
    assert_eq!(
        permits(&fixture).await,
        [("live".into(), live), ("user".into(), Acquirer::User)]
    );
}

/// A permit whose release hook fails becomes the user's, who is told how to
/// release it, so a later pass does not run the failing hook again.
#[tokio::test]
async fn a_failing_release_hook_keeps_the_permit_as_the_users_and_tells_the_user() {
    let fixture = Fixture::new(POOL, "").await;
    let space = SpaceId::generate();
    acquire(&fixture, "agent", Some(&agent(&space))).await;
    let hook = StandInHook {
        executable: "release.sh".into(),
        fails: true,
    };
    let hooks = &fixture.daemon.hooks;
    hooks
        .lock()
        .unwrap()
        .insert(HookKind::PreResourceRelease, hook);

    for _ in 0..2 {
        (fixture
            .leases
            .release_leases_of_ended_executions(&fixture.daemon))
        .await
        .unwrap();
    }

    assert_eq!(permits(&fixture).await, [("agent".into(), Acquirer::User)]);
    assert_eq!(fixture.daemon.hooks_run.lock().unwrap().len(), 1);
    let notified = fixture.notified().await;
    let failed: Vec<_> = (notified.iter())
        .filter(|(kind, _)| *kind == AlertKind::ReleaseFailed)
        .collect();
    assert_eq!(failed.len(), 1);
    assert!(
        failed[0]
            .1
            .contains("shoal release resource lock --name agent --space 1")
    );
}
