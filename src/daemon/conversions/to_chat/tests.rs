use crate::client::execution_ids::ExecutionId;
use crate::client::progress::ProgressChange;
use crate::client::space_names::SpaceSelector;
use crate::execution::recovery::LastSessionExt;
use crate::{
    daemon::{
        conversions::record::ConvertedChat,
        test_support::{git, manager, repository},
        workspace::Manager,
    },
    model::{HoldPlacer, Repository},
    state::ChatRestore,
};

async fn registered(root: &std::path::Path, manager: &Manager) -> Repository {
    let path = repository(root, "repo");
    manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

/// The refusal of converting the workspace `id` into a chat.
async fn refusal(manager: &Manager, id: &crate::client::space_ids::SpaceId) -> String {
    let refused = manager
        .convert_to_chat(&SpaceSelector::id(id), crate::model::chat_mode(), None)
        .await
        .unwrap_err();
    format!("{refused:#}")
}

#[tokio::test]
async fn a_workspace_the_user_holds_stays_one() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    (manager.acquire_hold(&selector, "keep".into(), None, HoldPlacer::User))
        .await
        .unwrap();

    let refused = refusal(&manager, &workspace.id).await;

    assert!(refused.contains("the user holds"), "{refused}");
}

#[tokio::test]
async fn a_workspace_another_builds_on_stays_one() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let base = manager
        .create_workspace(&repo.id, "base".into(), None, None, None)
        .await
        .unwrap();
    let stacked = manager
        .create_workspace(&repo.id, "stacked".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&stacked.id);
    (manager.set_base_workspace(&selector, Some(base.id.to_string())))
        .await
        .unwrap();

    let refused = refusal(&manager, &base.id).await;

    assert!(
        refused.contains("builds on the branch of base"),
        "{refused}"
    );
}

#[tokio::test]
async fn a_workspace_without_a_running_agent_has_none_to_carry_on() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();

    let refused = refusal(&manager, &workspace.id).await;

    assert!(refused.contains("no agent runs"), "{refused}");
}

#[tokio::test]
async fn a_chat_is_a_chat_already() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let (talk, _talk) =
        crate::daemon::test_support::chat(&manager, "talk", "chat", Some(&repo), None).await;

    let refused = refusal(&manager, &talk.id).await;

    assert!(refused.contains("is a chat already"), "{refused}");
}

#[tokio::test]
async fn the_worktree_goes_with_a_checkpoint_and_the_space_stays_a_chat() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&workspace.id),
            "part".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    std::fs::write(workspace.path.join("notes.txt"), "unsaved").unwrap();
    let status = ProgressChange::Status(Some("Merged".into()));
    (manager.set_progress(&SpaceSelector::id(&workspace.id), status))
        .await
        .unwrap();
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let identity = crate::process::identity::capture(process.id())
        .unwrap()
        .unwrap();
    process.kill();
    let execution = ExecutionId::generate();
    let carried_on = crate::execution::recovery::LastSession {
        execution: execution.clone(),
        agent: "claude".into(),
        session: Some("s1".into()),
        moved: false,
        copied: None,
        converted: false,
        origin: None,
        stopped_by: None,
        stop: None,
        model: Default::default(),
        mode: crate::config::modes::ModeName::DEFAULT,
        narrowing: Default::default(),
    };
    carried_on.write(&manager.paths, &workspace.id).unwrap();
    let chat = ConvertedChat {
        agent: "claude".into(),
        execution,
        session: Some("s1".into()),
        origin: None,
        process: identity,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
        stopped_by: None,
    };

    manager
        .convert_worktree_to_chat(&workspace.id, &chat)
        .await
        .unwrap();

    assert!(!workspace.path.exists());
    let checkout = repo.path.clone();
    git(
        &checkout,
        &["rev-parse", "--verify", "refs/heads/fix-login"],
    );
    assert!(manager.workspace_by_id(&workspace.id).await.is_err());
    let converted = manager.chat_by_id(&workspace.id).await.unwrap().unwrap();
    assert_eq!(
        (converted.number, &converted.name),
        (workspace.number, &workspace.name)
    );
    assert_eq!(converted.restore, Some(ChatRestore::Resume));
    assert!(converted.converted);
    assert_eq!(
        converted.session.map(|session| session.to_string()),
        Some("s1".into())
    );
    let status = converted.progress.and_then(|progress| progress.status);
    assert_eq!(status.as_deref(), Some("Merged"));
    let children: Vec<_> = converted.children.iter().map(|child| &child.id).collect();
    assert_eq!(children, [&child.id]);
    let removed = manager.removed_workspaces().await.unwrap();
    let record = removed
        .iter()
        .find(|removed| removed.workspace_id == workspace.id)
        .expect("a removal record restores the files");
    assert!(record.checkpoint.commit != record.checkpoint.head);
    // The chat carries the agent on, so a restore does not bring it back.
    assert!(
        record.last_sessions.is_empty(),
        "{:?}",
        record.last_sessions
    );
}

#[tokio::test]
async fn a_converted_workspace_restores_beside_the_chat_it_became() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    std::fs::write(workspace.path.join("notes.txt"), "unsaved").unwrap();
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let identity = crate::process::identity::capture(process.id())
        .unwrap()
        .unwrap();
    process.kill();
    let chat = ConvertedChat {
        agent: "claude".into(),
        execution: ExecutionId::generate(),
        session: None,
        origin: None,
        process: identity,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
        stopped_by: None,
    };
    (manager.convert_worktree_to_chat(&workspace.id, &chat))
        .await
        .unwrap();

    let restored = manager
        .restore_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();

    let reopened = restored.opened.workspace;
    assert_ne!(reopened.name, workspace.name);
    assert_eq!(reopened.branch.as_deref(), Some("fix-login"));
    assert!(reopened.path.join("notes.txt").exists());
    assert!(manager.chat_by_id(&workspace.id).await.unwrap().is_some());
}

#[tokio::test]
async fn a_conversion_of_a_workspace_whose_agent_stopped_is_not_recorded() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let identity = crate::process::identity::capture(process.id())
        .unwrap()
        .unwrap();
    process.kill();
    let chat = ConvertedChat {
        agent: "claude".into(),
        execution: ExecutionId::generate(),
        session: None,
        origin: None,
        process: identity,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
        stopped_by: None,
    };
    let id = workspace.id.clone();

    let refused = (manager.store)
        .write(move |tx| crate::daemon::conversions::record::record_chat_conversion(tx, &id, chat))
        .await
        .unwrap_err();

    assert!(
        format!("{refused:#}").contains("stopped meanwhile"),
        "{refused:#}"
    );
    assert!(!manager.converting(&workspace.id).await.unwrap());
}

#[tokio::test]
async fn a_workspace_built_on_after_the_command_keeps_its_worktree() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let base = manager
        .create_workspace(&repo.id, "base".into(), None, None, None)
        .await
        .unwrap();
    let stacked = manager
        .create_workspace(&repo.id, "stacked".into(), None, None, None)
        .await
        .unwrap();
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let identity = crate::process::identity::capture(process.id())
        .unwrap()
        .unwrap();
    process.kill();
    let chat = ConvertedChat {
        agent: "claude".into(),
        execution: ExecutionId::generate(),
        session: None,
        origin: None,
        process: identity,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
        stopped_by: None,
    };
    // Built on once the conversion was accepted, before the agent's turn ended.
    let selector = SpaceSelector::id(&stacked.id);
    (manager.set_base_workspace(&selector, Some(base.id.to_string())))
        .await
        .unwrap();

    let refused = manager
        .convert_worktree_to_chat(&base.id, &chat)
        .await
        .unwrap_err();

    assert!(
        format!("{refused:#}").contains("builds on its branch"),
        "{refused:#}"
    );
    assert!(base.path.exists());
    assert!(manager.chat_by_id(&base.id).await.unwrap().is_none());
}
