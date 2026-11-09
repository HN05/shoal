use crate::client::space_names::SpaceSelector;
use crate::{
    config::permissions::PermissionsConfig,
    daemon::test_support::{manager, repository},
    execution::{
        TrackedAgent,
        recovery::{LastSession, LastSessionExt},
    },
    protocol::SavedAgent,
};

#[tokio::test]
async fn a_narrowing_replaces_no_agent_when_one_is_refused() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (mut agents, mut stopped_id) = (Vec::new(), None);
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
        }
    }
    let stopped = |manager: &crate::daemon::workspace::Manager| {
        LastSession::all(&manager.paths, stopped_id.as_ref().unwrap()).unwrap()[0].narrowing
    };
    let narrowing = PermissionsConfig::assigned(&["push=propose".into()]).unwrap();

    // The second workspace saved no agent, so the first keeps its own.
    assert!(
        manager
            .replace_stopped_narrowing(&agents, narrowing)
            .await
            .is_err()
    );
    assert_eq!(stopped(&manager), PermissionsConfig::default());
    manager
        .replace_stopped_narrowing(&agents[..1], narrowing)
        .await
        .unwrap();
    assert_eq!(stopped(&manager), narrowing);
}
