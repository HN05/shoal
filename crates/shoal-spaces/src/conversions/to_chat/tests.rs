use crate::execution::recovery::LastSessionExt;
use crate::stand_in::Fixture;
use crate::{
    conversions::record::ConvertedChat,
    model::{HoldPlacer, Repository},
};
use shoal_core::test_support::{git, repository};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::progress::ProgressChange;
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::BranchName;

async fn registered(root: &std::path::Path, manager: &Fixture) -> Repository {
    let path = repository(root, "repo");
    manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

/// The refusal of converting the workspace `id` into a chat.
async fn refusal(manager: &Fixture, id: &shoal_ids::space_ids::SpaceId) -> String {
    let refused = manager
        .spaces
        .convert_to_chat(
            &**manager,
            &SpaceSelector::id(id),
            crate::model::chat_mode(),
        )
        .await
        .unwrap_err();
    format!("{refused:#}")
}

#[tokio::test]
async fn a_workspace_the_user_holds_stays_one() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    (manager
        .spaces
        .acquire_hold(&*manager, &selector, "keep".into(), None, HoldPlacer::User))
    .await
    .unwrap();

    let refused = refusal(&manager, &workspace.id).await;

    assert!(refused.contains("the user holds"), "{refused}");
}

#[tokio::test]
async fn a_workspace_another_builds_on_stays_one() {
    let manager = Fixture::new().await;
    let root = &manager.root;
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
    (manager
        .spaces
        .set_base_workspace(&selector, Some(base.id.to_string())))
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
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();

    let refused = refusal(&manager, &workspace.id).await;

    assert!(refused.contains("no agent runs"), "{refused}");
}

#[tokio::test]
async fn the_worktree_goes_with_a_checkpoint_and_the_space_stays_a_chat() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            "part".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    std::fs::write(workspace.path.join("notes.txt"), "unsaved").unwrap();
    let status = ProgressChange::Status(Some("Merged".into()));
    (manager
        .spaces
        .set_progress(&*manager, &SpaceSelector::id(&workspace.id), status))
    .await
    .unwrap();
    let execution = ExecutionId::generate();
    let carried_on = crate::execution::recovery::LastSession {
        turn: None,
        execution: execution.clone(),
        agent: "claude".into(),
        session: Some("s1".into()),
        moved: false,
        copied: None,
        converted: false,
        origin: None,
        stop: None,
        model: Default::default(),
        mode: crate::config::modes::ModeName::DEFAULT,
        narrowing: Default::default(),
    };
    carried_on.write(&manager.paths, &workspace.id).unwrap();
    let chat = ConvertedChat {
        agent: "claude".into(),
        execution,
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };

    manager
        .spaces
        .convert_worktree_to_chat(&*manager, &workspace.id, &chat)
        .await
        .unwrap();

    assert!(!workspace.path.exists());
    let checkout = repo.path.clone();
    git(
        &checkout,
        &["rev-parse", "--verify", "refs/heads/fix-login"],
    );
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
    let converted = manager
        .spaces
        .chat_by_id(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (converted.number, &converted.name),
        (workspace.number, &workspace.name)
    );
    assert!(!converted.running);
    let status = converted.progress.and_then(|progress| progress.status);
    assert_eq!(status.as_deref(), Some("Merged"));
    let children: Vec<_> = converted.children.iter().map(|child| &child.id).collect();
    assert_eq!(children, [&child.id]);
    let removed = manager.spaces.removed_workspaces().await.unwrap();
    let record = removed
        .iter()
        .find(|removed| removed.name == workspace.name)
        .expect("a removal record restores the files");
    assert!(
        record.kept_workspace().unwrap().checkpoint.commit
            != record.kept_workspace().unwrap().checkpoint.head
    );
    // The chat carries the agent on, so a restore does not bring it back.
    assert!(
        record.last_sessions.is_empty(),
        "{:?}",
        record.last_sessions
    );
}

#[tokio::test]
async fn a_workspace_that_becomes_a_chat_keeps_only_the_chats_own_token() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let exported = (manager
        .spaces
        .issue_scope(&SpaceSelector::id(&workspace.id)))
    .await
    .unwrap();
    let chat = ConvertedChat {
        agent: "claude".into(),
        execution: ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };

    (manager
        .spaces
        .convert_worktree_to_chat(&*manager, &workspace.id, &chat))
    .await
    .unwrap();

    assert!(
        manager
            .coordination
            .caller(&*manager, &exported)
            .await
            .unwrap()
            .is_none()
    );
    let token = manager.spaces.chat_scope(&workspace.id).await.unwrap();
    let caller = manager
        .coordination
        .caller(&*manager, &token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(caller.space_id, workspace.id);
}

#[tokio::test]
async fn a_converted_workspace_restores_beside_the_chat_it_became() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    std::fs::write(workspace.path.join("notes.txt"), "unsaved").unwrap();
    let chat = ConvertedChat {
        agent: "claude".into(),
        execution: ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    (manager
        .spaces
        .convert_worktree_to_chat(&*manager, &workspace.id, &chat))
    .await
    .unwrap();

    // Its record has an ID of its own, the chat keeping the workspace's.
    let removed = manager.spaces.removed_workspaces().await.unwrap();
    let [record] = removed.as_slice() else {
        panic!("{removed:?}");
    };
    assert_ne!(record.space_id, workspace.id);
    let restored = manager
        .restore_workspace(&SpaceSelector::id(&record.space_id))
        .await
        .unwrap();

    // Names repeat, so it keeps its name beside the chat that has it too.
    let reopened = restored.opened.workspace;
    assert_eq!(reopened.name, workspace.name);
    assert_eq!(reopened.branch().map(BranchName::as_str), Some("fix-login"));
    assert!(reopened.path.join("notes.txt").exists());
    assert!(
        manager
            .spaces
            .chat_by_id(&workspace.id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_conversion_of_a_workspace_whose_agent_stopped_is_not_recorded() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let chat = ConvertedChat {
        agent: "claude".into(),
        execution: ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    let id = workspace.id.clone();

    let refused = (manager.store)
        .write(move |tx| crate::conversions::record::record_chat_conversion(tx, &id, chat))
        .await
        .unwrap_err();

    assert!(
        format!("{refused:#}").contains("stopped meanwhile"),
        "{refused:#}"
    );
    assert!(!manager.spaces.converting(&workspace.id).await.unwrap());
}

#[tokio::test]
async fn a_workspace_built_on_after_the_command_keeps_its_worktree() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = registered(root.path(), &manager).await;
    let base = manager
        .create_workspace(&repo.id, "base".into(), None, None, None)
        .await
        .unwrap();
    let stacked = manager
        .create_workspace(&repo.id, "stacked".into(), None, None, None)
        .await
        .unwrap();
    let chat = ConvertedChat {
        agent: "claude".into(),
        execution: ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    // Built on once the conversion was accepted, before the agent's turn ended.
    let selector = SpaceSelector::id(&stacked.id);
    (manager
        .spaces
        .set_base_workspace(&selector, Some(base.id.to_string())))
    .await
    .unwrap();

    let refused = manager
        .spaces
        .convert_worktree_to_chat(&*manager, &base.id, &chat)
        .await
        .unwrap_err();

    assert!(
        format!("{refused:#}").contains("builds on its branch"),
        "{refused:#}"
    );
    assert!(base.path.exists());
    assert!(manager.spaces.chat_by_id(&base.id).await.unwrap().is_none());
}

#[tokio::test]
async fn nothing_builds_on_a_workspace_that_becomes_a_chat() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = registered(root.path(), &manager).await;
    let base = manager
        .create_workspace(&repo.id, "base".into(), None, None, None)
        .await
        .unwrap();
    let stacked = manager
        .create_workspace(&repo.id, "stacked".into(), None, None, None)
        .await
        .unwrap();
    let chat = ConvertedChat {
        agent: "claude".into(),
        execution: ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    let (id, pending) = (
        base.id.clone(),
        crate::conversions::Pending::Chat {
            chat: Box::new(chat),
            held: false,
        },
    );
    (manager.store)
        .write(move |tx| crate::conversions::record::record(tx, &id, &pending))
        .await
        .unwrap();

    let selector = SpaceSelector::id(&stacked.id);
    let refused = (manager
        .spaces
        .set_base_workspace(&selector, Some(base.id.to_string())))
    .await
    .unwrap_err();
    let created = manager
        .create_workspace(&repo.id, "on-base".into(), Some("base".into()), None, None)
        .await
        .unwrap();

    assert!(
        format!("{refused:#}").contains("becomes a chat"),
        "{refused:#}"
    );
    assert!(created.base_workspace.is_none());
}

#[tokio::test]
async fn a_failed_conversion_leaves_the_workspace_its_record_and_checkpoints() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let repo = registered(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    std::fs::write(workspace.path.join("notes.txt"), "unsaved").unwrap();
    let chat = ConvertedChat {
        agent: "claude".into(),
        execution: ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    // Git refuses to remove a locked worktree, which fails the conversion
    // after its record is saved.
    let path = workspace.path.to_str().unwrap();
    git(&repo.path, &["worktree", "lock", path]);
    let converted = (manager
        .spaces
        .convert_worktree_to_chat(&*manager, &workspace.id, &chat))
    .await;
    git(&repo.path, &["worktree", "unlock", path]);
    assert!(converted.is_err());

    // Nothing to restore while the workspace exists, which keeps its
    // checkpoints.
    assert!(manager.spaces.removed_spaces().await.unwrap().is_empty());
    let checkpoints = manager
        .spaces
        .checkpoints(&SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap()
        .unwrap();
    assert!(!checkpoints.is_empty());
}
