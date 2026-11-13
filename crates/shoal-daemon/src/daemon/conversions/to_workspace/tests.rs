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
    protocol::OperationMethod,
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

/// An agent converts the spaces its scope reaches, its own and those below
/// it, and is refused one outside, which the refusal names.
#[tokio::test]
async fn an_agent_converts_the_spaces_its_scope_reaches() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let lead = chat(&manager, "lead", "chat", Some(&repo), None).await;
    let talk = chat(&manager, "talk", "worker", Some(&repo), Some(&lead.id)).await;
    let other = chat(&manager, "other", "worker", Some(&repo), None).await;
    let caller = Caller {
        space_id: lead.id.clone(),
        execution: None,
    };
    manager.executions.issue_scope("lead".into(), caller).await;

    for (chat, allowed) in [(&lead, true), (&talk, true), (&other, false)] {
        let mut convert = OperationMethod::ConvertToWorkspace {
            chat: SpaceSelector::id(&chat.id),
            branch: "fix-login".into(),
            issue: None,
            base: None,
            options: Default::default(),
            mode: None,
            env: None,
        };
        let authorized =
            crate::daemon::scope::authorize(&manager, Some(&"lead".into()), &mut convert).await;
        match authorized {
            Ok(_) => assert!(allowed, "{}", chat.name),
            Err(error) => {
                assert!(!allowed, "{}: {error:#}", chat.name);
                assert!(format!("{error:#}").contains("outside this space's scope"));
            }
        }
    }
}

#[tokio::test]
async fn a_chat_in_a_workspaces_worktree_stays_a_chat() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let inside = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "inside".into(),
            "claude".into(),
            None,
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
    let talk = chat(&manager, "talk", "worker", Some(&repo), None).await;
    manager.end_executions(&talk.id).await;

    let refused = refusal(&manager, SpaceSelector::id(&talk.id), None).await;

    assert!(refused.contains("is stopped"), "{refused}");
}

#[tokio::test]
async fn a_mode_no_config_defines_is_refused_before_anything_changes() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let talk = chat(&manager, "talk", "worker", Some(&repo), None).await;
    let conversion = WorkspaceConversion {
        mode: "missing".into(),
        ..conversion()
    };

    let refused = manager
        .convert_to_workspace(&SpaceSelector::id(&talk.id), conversion, None, None)
        .await
        .unwrap_err();

    assert!(
        format!("{refused:#}").contains("no mode missing"),
        "{refused:#}"
    );
    assert!(manager.chat_by_id(&talk.id).await.unwrap().is_some());
}
