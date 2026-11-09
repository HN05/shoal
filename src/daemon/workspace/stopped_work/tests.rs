use crate::client::space_names::SpaceSelector;
use crate::{
    config::permissions::{PermissionChange, PermissionsConfig},
    daemon::test_support::{chat, manager, repository},
    execution::{
        TrackedAgent,
        recovery::{LastSession, LastSessionExt},
    },
    protocol::SavedAgent,
};

#[tokio::test]
async fn a_permission_change_changes_no_agent_or_chat_when_one_is_refused() {
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
    let (talk, _process) = chat(&manager, "talk", "chat", Some(&repo), None).await;
    let chats = [SpaceSelector::id(&talk.id)];

    // The second workspace saved no agent, so the first keeps its own.
    let changed = manager.change_stopped_permissions(&agents, &[], &change, None);
    assert!(changed.await.is_err());
    assert_eq!(stopped(&manager), PermissionsConfig::default());
    // A running chat is not saved, so the agent keeps its own too.
    let changed = manager.change_stopped_permissions(&agents[..1], &chats, &change, None);
    assert!(changed.await.is_err());
    assert_eq!(stopped(&manager), PermissionsConfig::default());
    // A space's process gives only what it holds: no workspace's mode merges
    // directly here.
    manager.save_chat(&talk.id, None).await.unwrap();
    let giver = running_id.as_ref();
    let changed = manager.change_stopped_permissions(&agents[..1], &chats, &change, giver);
    assert!(changed.await.is_err());
    assert_eq!(stopped(&manager), PermissionsConfig::default());
    let saved = manager.chat_by_id(&talk.id).await.unwrap().unwrap();
    assert_eq!(saved.narrowing, PermissionsConfig::default());
    // Chats change all or none: one restored since refuses the rest.
    let (running, _running) = chat(&manager, "busy", "chat", Some(&repo), None).await;
    let both = vec![(talk.id.clone(), change.set), (running.id, change.set)];
    assert!(manager.give_saved_chats(both).await.is_err());
    let saved = manager.chat_by_id(&talk.id).await.unwrap().unwrap();
    assert_eq!(saved.narrowing, PermissionsConfig::default());
    // The user gives any, to a saved chat as to a stopped agent.
    (manager.change_stopped_permissions(&agents[..1], &chats, &change, None))
        .await
        .unwrap();
    assert_eq!(stopped(&manager), change.set);
    let saved = manager.chat_by_id(&talk.id).await.unwrap().unwrap();
    assert_eq!(saved.narrowing, change.set);
}
