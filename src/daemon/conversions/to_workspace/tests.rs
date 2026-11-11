use super::WorkspaceConversion;
use crate::client::space_names::SpaceSelector;
use crate::{
    config::modes::ModeName,
    daemon::{
        scope::Caller,
        test_support::{chat, manager, repository},
        workspace::Manager,
    },
    model::Repository,
};

async fn registered(root: &std::path::Path, manager: &Manager) -> Repository {
    let path = repository(root, "repo");
    manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

fn conversion() -> WorkspaceConversion {
    WorkspaceConversion {
        branch: "fix-login".into(),
        issue: None,
        base: None,
        options: Default::default(),
        mode: ModeName::DEFAULT,
    }
}

/// The refusal of converting the space `selector` names for `caller`.
async fn refusal(
    manager: &std::sync::Arc<Manager>,
    selector: SpaceSelector,
    caller: Option<&Caller>,
) -> String {
    let refused = manager
        .convert_to_workspace(&selector, conversion(), None, caller)
        .await
        .unwrap_err();
    format!("{refused:#}")
}

#[tokio::test]
async fn a_workspace_is_a_workspace_already() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();

    let refused = refusal(&manager, SpaceSelector::id(&workspace.id), None).await;

    assert!(refused.contains("is a workspace already"), "{refused}");
}

#[tokio::test]
async fn an_agent_converts_only_its_own_chat() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let (lead, _lead) = chat(&manager, "lead", "chat", Some(&repo), None).await;
    let (talk, _talk) = chat(&manager, "talk", "worker", Some(&repo), Some(&lead.id)).await;
    let caller = Caller {
        space_id: lead.id.clone(),
        execution: None,
    };

    let refused = refusal(&manager, SpaceSelector::id(&talk.id), Some(&caller)).await;

    assert!(refused.contains("only its own space"), "{refused}");
    assert!(manager.chat_by_id(&talk.id).await.unwrap().is_some());
}

#[tokio::test]
async fn a_chat_in_a_workspaces_worktree_stays_a_chat() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let inside = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "inside".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();

    let refused = refusal(&manager, SpaceSelector::id(&inside.id), None).await;

    assert!(refused.contains("worktree of worker"), "{refused}");
}

#[tokio::test]
async fn a_stopped_chat_is_resumed_before_it_converts() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let (talk, process) = chat(&manager, "talk", "worker", Some(&repo), None).await;
    process.kill();
    manager.stop_finished_chats().await.unwrap();

    let refused = refusal(&manager, SpaceSelector::id(&talk.id), None).await;

    assert!(refused.contains("is stopped"), "{refused}");
}
