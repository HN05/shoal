use crate::client::space_names::SpaceSelector;
use crate::{
    config::permissions::{PermissionChange, PermissionsConfig},
    daemon::test_support::{manager, repository},
    execution::{
        TrackedAgent,
        recovery::{LastSession, LastSessionExt},
    },
    protocol::SavedAgent,
};

#[tokio::test]
async fn a_permission_change_changes_no_agent_when_one_is_refused() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (mut agents, mut stopped_id, mut running_id) = (Vec::new(), None, None);
    for name in ["stopped", "running"] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        agents.push(SavedAgent {
            workspace: SpaceSelector::id(&workspace.id),
            execution: format!("{name}-agent"),
        });
        if name == "stopped" {
            let agent = TrackedAgent::named("claude");
            LastSession::save(&manager.paths, &workspace.id, &agent, "stopped-agent").unwrap();
            stopped_id = Some(workspace.id);
        } else {
            running_id = Some(workspace.id);
        }
    }
    let stopped = |manager: &crate::daemon::workspace::Manager| {
        LastSession::all(&manager.paths, stopped_id.as_ref().unwrap()).unwrap()[0].narrowing
    };
    let change = PermissionChange::assigned("merge=direct").unwrap();

    // The second workspace saved no agent, so the first keeps its own.
    let changed = manager.change_stopped_permissions(&agents, &change, None);
    assert!(changed.await.is_err());
    assert_eq!(stopped(&manager), PermissionsConfig::default());
    // A space's process gives only what it holds: no workspace's mode merges
    // directly here.
    let giver = running_id.as_ref();
    let changed = manager.change_stopped_permissions(&agents[..1], &change, giver);
    assert!(changed.await.is_err());
    assert_eq!(stopped(&manager), PermissionsConfig::default());
    // The user gives any.
    (manager.change_stopped_permissions(&agents[..1], &change, None))
        .await
        .unwrap();
    assert_eq!(stopped(&manager), change.set);
}
