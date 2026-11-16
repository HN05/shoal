//! The containers an agent's start brings up in its workspace.
use crate::{
    client::connection::{CommandPaths, Connector},
    model::{ExecutionPlan, SpaceKind},
};
use shoal_protocol::space_names::SpaceSelector;

/// Give the agent of `plan` its workspace's containers before it starts,
/// starting them where its repository starts them with setup. A failure
/// only warns: the agent starts, and `shoal acquire containers` retries.
pub(super) async fn start_for_agent(paths: &CommandPaths, plan: &ExecutionPlan) {
    if plan.space.kind != SpaceKind::Workspace {
        return;
    }
    // As the agent, which the containers then end with.
    let agent = Connector {
        paths: (**paths).clone(),
        scope: Some(plan.scope_token.clone()),
    };
    let workspace = SpaceSelector::id(&plan.space.id);
    let started = crate::client::leases::start_agent_containers(&agent, workspace).await;
    if let Err(error) = started {
        eprintln!(
            "warning: the workspace's containers did not start: {error:#}; \
             `shoal acquire containers` retries"
        );
    }
}
