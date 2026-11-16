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
