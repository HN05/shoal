use super::*;
use crate::hooks::HookKind;
use crate::resources::ResourceRequest;
use crate::stand_in::{Fixture, StandInHook};

const POOL: &str = "[resources.lock]\ncapacity = 4\n";

/// An agent's caller of the fixture's workspace, running `execution`.
fn agent(fixture: &Fixture, execution: &ExecutionId) -> Caller {
    let mut caller = Caller::of_space(fixture.workspace.id.clone());
    caller.execution = Some(crate::callers::ScopedExecution {
        id: execution.clone(),
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

/// The sweep releases the permits of an agent whose execution ended or runs
/// no process, keeps those of a running agent and the user's.
#[tokio::test]
async fn the_sweep_releases_the_permits_of_ended_agents_alone() {
    let fixture = Fixture::new(POOL, "").await;
    let [ended, stopped, running] = [(); 3].map(|()| ExecutionId::generate());
    for (name, execution) in [
        ("ended", &ended),
        ("stopped", &stopped),
        ("running", &running),
    ] {
        acquire(&fixture, name, Some(&agent(&fixture, execution))).await;
    }
    acquire(&fixture, "user", None).await;
    *fixture.daemon.running.lock().unwrap() = [(stopped, false), (running.clone(), true)].into();

    (fixture
        .leases
        .release_leases_of_ended_executions(&fixture.daemon))
    .await
    .unwrap();

    let running = Acquirer::Agent {
        execution_id: running,
    };
    assert_eq!(
        permits(&fixture).await,
        [("running".into(), running), ("user".into(), Acquirer::User)]
    );
}

/// A permit whose release hook fails becomes the user's, who is told how to
/// release it, so a later pass does not run the failing hook again.
#[tokio::test]
async fn a_failing_release_hook_keeps_the_permit_as_the_users_and_tells_the_user() {
    let fixture = Fixture::new(POOL, "").await;
    let execution = ExecutionId::generate();
    acquire(&fixture, "agent", Some(&agent(&fixture, &execution))).await;
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
