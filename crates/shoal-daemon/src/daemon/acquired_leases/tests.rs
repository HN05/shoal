use std::{sync::Arc, time::Duration};

use shoal_ids::execution_ids::ExecutionId;

use super::*;
use crate::{
    daemon::test_support::{configure, manager, repository},
    model::Workspace,
};
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
                &crate::execution::TrackedAgent::named("claude"),
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
            .acquire_resource(&self.selector(), request, caller)
            .await
            .unwrap();
        assert!(
            matches!(acquired, Allocation::Granted(_)),
            "{name} not granted"
        );
    }

    /// The workspace's permits by name, with who acquired each.
    async fn permits(&self) -> Vec<(String, Acquirer)> {
        let leases = self.manager.list_resources(Some(&self.selector())).await;
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
        let sim = shoal_protocol::sim::Simulator {
            id: name.into(),
            udid: None,
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
        let sims = self.manager.list_simulators(Some(&self.workspace.id));
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

    fixture.manager.release_agent_leases(&agent).await;
    fixture.manager.release_agent_leases(&demo).await;

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

#[tokio::test]
async fn a_failing_release_hook_keeps_the_permit_and_tells_the_user() {
    let config = format!("hooks.pre_resource_release = 'release.sh'\n{POOL}");
    let fixture = Fixture::new(&config).await;
    let hook = fixture.workspace.path.join("release.sh");
    std::fs::write(&hook, "#!/bin/sh\necho 'device in use' >&2\nexit 1\n").unwrap();
    let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&hook, permissions).unwrap();
    let (execution_id, caller) = fixture.agent().await;
    fixture.acquire("agent", Some(&caller)).await;

    fixture
        .manager
        .end_test_execution(execution_id.clone())
        .await;

    let agent = Acquirer::Agent { execution_id };
    assert_eq!(fixture.permits().await, [permit("agent", &agent)]);
    let notices = fixture.manager.notifications(false, 10).await.unwrap();
    let failed: Vec<_> = (notices.iter())
        .filter(|notice| notice.kind == NotificationKind::ReleaseFailed)
        .collect();
    assert_eq!(failed.len(), 1);
    assert!(
        failed[0]
            .message
            .contains("shoal release resource lock --name agent")
    );
}

#[tokio::test]
async fn leases_of_executions_that_ended_unreleased_are_released() {
    let fixture = Fixture::new(POOL).await;
    let ended = Acquirer::Agent {
        execution_id: ExecutionId::generate(),
    };
    fixture.lease_simulator("ended", ended).await;
    let (execution_id, _) = fixture.agent().await;
    let running = Acquirer::Agent { execution_id };
    fixture.lease_simulator("running", running).await;

    fixture.manager.release_leases_of_ended_executions().await;

    assert_eq!(fixture.simulators().await, ["running"]);
}
