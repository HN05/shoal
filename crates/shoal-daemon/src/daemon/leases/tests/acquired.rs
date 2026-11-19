//! Leases that end with the agents and demos that acquired them, as the
//! daemon tracks their executions.
use std::{sync::Arc, time::Duration};

use shoal_ids::execution_ids::ExecutionId;

use crate::daemon::{
    allocation::Allocation,
    manager::Manager,
    scope::Caller,
    test_support::{agent_process, configure, manager, repository},
};
use crate::{model::Workspace, refusal::Reason};
use shoal_protocol::acquirers::Acquirer;
use shoal_protocol::notifications::NotificationKind;
use shoal_protocol::{resources::ResourceRequest, space_names::SpaceSelector};

/// A ready workspace in a daemon whose global config is `config`.
struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    workspace: Workspace,
}

impl Fixture {
    async fn new(config: &str) -> Self {
        let (root, manager) = manager().await;
        configure(&manager, config).await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "worker".into(), None, None, None)
            .await
            .unwrap();
        Self {
            _root: root,
            manager,
            workspace,
        }
    }

    fn selector(&self) -> SpaceSelector {
        SpaceSelector::id(&self.workspace.id)
    }

    /// Start a tracked agent in the workspace, returning its execution and
    /// the caller its requests come as.
    async fn agent(&self) -> (ExecutionId, Caller) {
        let execution = (self.manager)
            .begin_agent_execution(self.workspace.id.as_str())
            .await
            .unwrap();
        let id = execution.plan.id.clone();
        self.manager
            .track_agent(
                &id,
                &shoal_protocol::executions::TrackedAgent::named("claude"),
                &self.workspace.id,
                &self.workspace.name,
                false,
                Duration::ZERO,
            )
            .await;
        let caller = (self.manager.caller(&execution.plan.scope_token))
            .await
            .unwrap()
            .unwrap();
        (id, caller)
    }

    /// Acquire a permit of the pool `lock` named `name` as `caller`.
    async fn acquire(&self, name: &str, caller: Option<&Caller>) {
        let request = ResourceRequest {
            mode: None,
            kind: None,
            pool: "lock".into(),
            name: name.into(),
            resource: None,
            reason: None,
            queue: false,
        };
        let acquired = self
            .manager
            .leases
            .acquire_resource(&*self.manager, &self.selector(), request, caller)
            .await
            .unwrap();
        assert!(
            matches!(acquired, Allocation::Granted(_)),
            "{name} not granted"
        );
    }

    /// The workspace's permits by name, with who acquired each.
    async fn permits(&self) -> Vec<(String, Acquirer)> {
        let leases = self
            .manager
            .leases
            .list_resources(&*self.manager, Some(&self.selector()))
            .await;
        let mut permits: Vec<_> = (leases.unwrap().into_iter())
            .map(|lease| (lease.name, lease.acquirer))
            .collect();
        permits.sort_by(|a, b| a.0.cmp(&b.0));
        permits
    }
}

const POOL: &str = "[resources.lock]\ncapacity = 3\n";

fn permit(name: &str, acquirer: &Acquirer) -> (String, Acquirer) {
    (name.to_owned(), acquirer.clone())
}

#[tokio::test]
async fn an_agents_permit_is_its_executions_and_the_users_the_users() {
    let fixture = Fixture::new(POOL).await;
    let (execution_id, caller) = fixture.agent().await;

    fixture.acquire("agent", Some(&caller)).await;
    fixture.acquire("user", None).await;

    let agent = Acquirer::Agent { execution_id };
    assert_eq!(
        fixture.permits().await,
        [permit("agent", &agent), permit("user", &Acquirer::User)]
    );
}

impl Fixture {
    /// Record a leased simulator named `name` that `acquirer` acquired,
    /// without a device, so releasing it runs no simctl.
    async fn lease_simulator(&self, name: &str, acquirer: Acquirer) {
        self.lease_device(name, None, acquirer).await;
    }

    /// Record a leased simulator named `name` on the device `udid`, which
    /// releasing it would read with simctl.
    async fn lease_device(&self, name: &str, udid: Option<&str>, acquirer: Acquirer) {
        let sim = shoal_protocol::sim::Simulator {
            id: name.into(),
            udid: udid.map(Into::into),
            device: "phone".into(),
            runtime: "ios".into(),
            workspace_id: Some(self.workspace.id.clone()),
            last_workspace_id: None,
            lease_name: Some(name.into()),
            reason: None,
            state: shoal_protocol::sim::SimulatorState::Leased,
            last_used: crate::time::UnixTime::now(),
            error: None,
            installed_apps: None,
            acquirer,
        };
        let record = serde_json::to_string(&sim).unwrap();
        (self.manager.store)
            .run(move |db| {
                db.execute(
                    "INSERT INTO simulators(id,record) VALUES (?1,?2)",
                    [&sim.id, &record],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// The names of the workspace's simulator leases.
    async fn simulators(&self) -> Vec<String> {
        let sims = self
            .manager
            .leases
            .list_simulators(Some(&self.workspace.id));
        let mut names: Vec<_> = (sims.await.unwrap().into_iter())
            .filter(|sim| sim.workspace_id.is_some())
            .filter_map(|sim| sim.lease_name)
            .collect();
        names.sort();
        names
    }
}

#[tokio::test]
async fn an_agents_leases_end_with_its_execution_and_the_users_stay() {
    let fixture = Fixture::new(POOL).await;
    let (execution_id, caller) = fixture.agent().await;
    fixture.acquire("agent", Some(&caller)).await;
    fixture.acquire("user", None).await;
    let agent = Acquirer::Agent {
        execution_id: execution_id.clone(),
    };
    fixture.lease_simulator("agent", agent).await;
    fixture.lease_simulator("user", Acquirer::User).await;

    fixture.manager.end_test_execution(execution_id).await;

    assert_eq!(fixture.permits().await, [permit("user", &Acquirer::User)]);
    assert_eq!(fixture.simulators().await, ["user"]);
}

/// A stop that keeps the execution for a restore ends its agent's leases,
/// while a demo's execution between runs keeps its own.
#[tokio::test]
async fn a_stopped_agents_leases_end_and_a_demo_keeps_its_own_between_runs() {
    let fixture = Fixture::new(POOL).await;
    let (agent, _) = fixture.agent().await;
    let demo = ExecutionId::generate();
    let agent_acquirer = Acquirer::Agent {
        execution_id: agent.clone(),
    };
    let demo_acquirer = Acquirer::Demo {
        execution_id: demo.clone(),
    };
    fixture.lease_simulator("agent", agent_acquirer).await;
    fixture.lease_simulator("demo", demo_acquirer).await;

    fixture
        .manager
        .leases
        .release_agent_leases(&*fixture.manager, &agent)
        .await;
    fixture
        .manager
        .leases
        .release_agent_leases(&*fixture.manager, &demo)
        .await;

    assert_eq!(fixture.simulators().await, ["demo"]);
}

#[tokio::test]
async fn the_users_acquire_keeps_an_agents_permit_past_its_exit() {
    let fixture = Fixture::new(POOL).await;
    let (execution_id, caller) = fixture.agent().await;
    fixture.acquire("shared", Some(&caller)).await;

    fixture.acquire("shared", None).await;
    fixture.manager.end_test_execution(execution_id).await;

    assert_eq!(fixture.permits().await, [permit("shared", &Acquirer::User)]);
}

/// The sweep releases the leases of an agent whose execution ended, or
/// that a stop left without a process while its execution waits for a
/// restore, and keeps those of a running agent and of a demo between runs.
#[tokio::test]
async fn leases_of_ended_and_stopped_agents_are_released_and_running_ones_kept() {
    let fixture = Fixture::new(POOL).await;
    let (running, _) = fixture.agent().await;
    let process = agent_process(&fixture.manager, &fixture.workspace.id).await;
    let (stopped, _) = fixture.agent().await;
    let demo = fixture.manager.begin_execution(
        fixture.workspace.id.as_str(),
        None,
        crate::daemon::executions::ExecutionKind::Command,
        None,
    );
    let demo = demo.await.unwrap().plan.id;
    let leases = [
        (
            "ended",
            Acquirer::Agent {
                execution_id: ExecutionId::generate(),
            },
        ),
        (
            "running",
            Acquirer::Agent {
                execution_id: running,
            },
        ),
        (
            "stopped",
            Acquirer::Agent {
                execution_id: stopped,
            },
        ),
        ("between-runs", Acquirer::Demo { execution_id: demo }),
    ];
    for (name, acquirer) in leases {
        fixture.lease_simulator(name, acquirer).await;
    }

    (fixture
        .manager
        .leases
        .release_leases_of_ended_executions(&*fixture.manager))
    .await
    .unwrap();

    assert_eq!(fixture.simulators().await, ["between-runs", "running"]);
    process.kill();
}

impl Fixture {
    /// Start the simulator demo `name` on the device `udid` as `caller`
    /// starts it, returning its execution and the caller its command's
    /// requests come as.
    async fn simulator_demo(
        &self,
        name: &str,
        udid: &str,
        caller: &Caller,
    ) -> (ExecutionId, Caller) {
        use crate::sessions::{HostName, Origin};
        use shoal_protocol::demos::{Launch, Preset, SIM_UDID};
        let launch = Launch {
            id: name.into(),
            name: name.into(),
            preset: Some(Preset::Simulator),
            open: None,
            scheme: None,
            environment: [(SIM_UDID.to_owned(), udid.to_owned())].into(),
            origin: Origin::created(HostName::Zmx, name.into()),
        };
        let kind = crate::daemon::executions::ExecutionKind::Command;
        let started = self.manager.begin_execution_running(
            self.workspace.id.as_str(),
            None,
            kind,
            Some(caller),
            Some(launch),
        );
        let started = started.await.unwrap();
        let demo = (self.manager.caller(&started.plan.scope_token))
            .await
            .unwrap()
            .unwrap();
        (started.plan.id, demo)
    }

    /// Who acquired each of the workspace's simulator leases, by name.
    async fn simulator_acquirers(&self) -> Vec<(String, Acquirer)> {
        let sims = self
            .manager
            .leases
            .list_simulators(Some(&self.workspace.id));
        let mut leases: Vec<_> = (sims.await.unwrap().into_iter())
            .filter_map(|sim| Some((sim.lease_name?, sim.acquirer)))
            .collect();
        leases.sort_by(|a, b| a.0.cmp(&b.0));
        leases
    }
}

/// A simulator demo an agent starts takes over the lease the agent took for
/// it, so it outlives the agent, while the user's lease stays the user's.
#[tokio::test]
async fn a_simulator_demo_keeps_the_lease_its_agent_took_past_the_agent() {
    let fixture = Fixture::new(POOL).await;
    let (agent, caller) = fixture.agent().await;
    let agent_acquirer = Acquirer::Agent {
        execution_id: agent.clone(),
    };
    fixture
        .lease_device("default", Some("DEMO"), agent_acquirer)
        .await;
    fixture
        .lease_device("user", Some("USER"), Acquirer::User)
        .await;

    let (demo, _) = fixture.simulator_demo("app", "DEMO", &caller).await;
    fixture.simulator_demo("other", "USER", &caller).await;
    fixture.manager.end_test_execution(agent).await;

    let demo = Acquirer::Demo { execution_id: demo };
    assert_eq!(
        fixture.simulator_acquirers().await,
        [permit("default", &demo), permit("user", &Acquirer::User)]
    );
}

/// What a demo's command acquires is the demo's, not the agent's that
/// started it.
#[tokio::test]
async fn a_demos_command_acquires_for_the_demo() {
    let fixture = Fixture::new(POOL).await;
    let (_, caller) = fixture.agent().await;
    let (demo, demo_caller) = fixture.simulator_demo("app", "DEMO", &caller).await;

    fixture.acquire("demo", Some(&demo_caller)).await;

    let demo = Acquirer::Demo { execution_id: demo };
    assert_eq!(fixture.permits().await, [permit("demo", &demo)]);
}

/// A lease whose agent ends while another lifecycle operation, such as a
/// stop, holds its workspace is released once the workspace is ready again.
#[tokio::test]
async fn an_agent_ending_during_a_stop_releases_its_leases_once_the_stop_ends() {
    let fixture = Fixture::new(POOL).await;
    let (execution_id, caller) = fixture.agent().await;
    fixture.acquire("agent", Some(&caller)).await;
    let manager = &fixture.manager;
    let stopping = crate::state::WorkspaceState::Stopping;
    let reserved = (manager.reserve_lifecycle(&fixture.workspace.id, stopping))
        .await
        .unwrap();

    manager.end_test_execution(execution_id.clone()).await;
    let agent = Acquirer::Agent { execution_id };
    assert_eq!(fixture.permits().await, [permit("agent", &agent)]);

    (manager.set_state(&fixture.workspace.id, reserved, None))
        .await
        .unwrap();
    manager
        .leases
        .release_ended_leases_of(&**manager, &fixture.workspace.id)
        .await
        .unwrap();
    assert_eq!(fixture.permits().await, []);
}

/// A release another resource operation holds off, such as a hook, stays
/// the agent's and goes on the next pass, rather than becoming the user's.
#[tokio::test]
async fn a_release_a_busy_resource_operation_holds_off_waits_for_the_next_pass() {
    let fixture = Fixture::new(POOL).await;
    let (execution_id, caller) = fixture.agent().await;
    fixture.acquire("agent", Some(&caller)).await;
    let manager = &fixture.manager;
    let hook = crate::daemon::gates::GuardMode::Exclusive;
    let hook = manager.gates.resource_guard(&fixture.workspace.id, hook);
    let hook = hook.unwrap();

    manager.end_test_execution(execution_id.clone()).await;
    let agent = Acquirer::Agent { execution_id };
    assert_eq!(fixture.permits().await, [permit("agent", &agent)]);

    drop(hook);
    manager
        .leases
        .release_leases_of_ended_executions(&**manager)
        .await
        .unwrap();
    assert_eq!(fixture.permits().await, []);
    let notices = manager.notifications.list(false, 10).await.unwrap();
    assert!(
        (notices.iter()).all(|notice| notice.kind != NotificationKind::ReleaseFailed),
        "no release failed"
    );
}

/// A simulator release that finds its workspace taken by a lifecycle
/// operation waits for the next pass, as a resource release does.
#[tokio::test]
async fn a_simulator_release_a_lifecycle_operation_holds_off_waits() {
    let fixture = Fixture::new(POOL).await;
    let (execution_id, _) = fixture.agent().await;
    let agent = Acquirer::Agent { execution_id };
    fixture.lease_simulator("agent", agent).await;
    let manager = &fixture.manager;
    let stopping = crate::state::WorkspaceState::Stopping;
    (manager.reserve_lifecycle(&fixture.workspace.id, stopping))
        .await
        .unwrap();

    let selector = fixture.selector();
    let released = manager
        .leases
        .release_simulator(&**manager, &selector, "agent".into())
        .await;

    let error = released.unwrap_err();
    assert_eq!(Reason::of(&error), Some(Reason::NotReady), "{error:#}");
}

/// A release decided for an agent's lease leaves it alone once the user
/// has taken it over since.
#[tokio::test]
async fn a_release_decided_for_an_agent_leaves_a_lease_the_user_took_over_since() {
    let fixture = Fixture::new(POOL).await;
    let (execution_id, caller) = fixture.agent().await;
    let agent = Acquirer::Agent { execution_id };
    fixture.acquire("permit", Some(&caller)).await;
    fixture.lease_simulator("sim", agent.clone()).await;
    let manager = &fixture.manager;
    fixture.acquire("permit", None).await;
    (manager
        .leases
        .give_simulator_lease_to_user(&fixture.workspace.id, "sim"))
    .await
    .unwrap();

    let selector = fixture.selector();
    let permit = manager.leases.release_resource_held_by(
        &**manager,
        &selector,
        "lock".into(),
        "permit".into(),
        Some(&agent),
    );
    permit.await.unwrap();
    let sim =
        manager
            .leases
            .release_simulator_held_by(&**manager, &selector, "sim".into(), Some(&agent));
    sim.await.unwrap();

    assert_eq!(
        fixture.permits().await,
        [self::permit("permit", &Acquirer::User)]
    );
    assert_eq!(fixture.simulators().await, ["sim"]);
}

/// Releasing an ended agent's leases is not use of its workspace, so idle
/// cleanup keeps counting from when the agent last worked.
#[tokio::test]
async fn releasing_an_ended_agents_leases_leaves_the_workspace_idle() {
    let fixture = Fixture::new(POOL).await;
    let (execution_id, caller) = fixture.agent().await;
    fixture.acquire("agent", Some(&caller)).await;
    let agent = Acquirer::Agent {
        execution_id: execution_id.clone(),
    };
    fixture.lease_simulator("agent", agent).await;
    let manager = &fixture.manager;
    let id = fixture.workspace.id.clone();
    (manager.store.run(move |db| {
        db.execute("UPDATE workspaces SET active_at=1 WHERE id=?1", [id])?;
        Ok(())
    }))
    .await
    .unwrap();

    manager
        .leases
        .release_agent_leases(&**manager, &execution_id)
        .await;

    assert_eq!(fixture.permits().await, []);
    assert_eq!(fixture.simulators().await, Vec::<String>::new());
    let active_at = manager.active_at(&fixture.workspace.id).await.unwrap();
    assert_eq!(active_at, Some(crate::time::UnixTime::from_seconds(1)));
}
