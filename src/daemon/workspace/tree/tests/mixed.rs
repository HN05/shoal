//! Trees that mix workspaces and chats: every walk passes spaces of either
//! kind alike.
use crate::{
    daemon::{
        approvals::{ApprovalRequest, Lifetime, SecretSpecification, Specification, Target},
        workspace::Manager,
        workspace_files::Attacher,
    },
    model::{Chat, SpaceKind, Workspace},
    removal::{InspectionPolicy, UnsavedWork},
    state::AgentState,
    test_support::{ProcessGroup, manager, repository},
};

/// A workspace `root` with an agent, a chat below it running as the
/// returned process, and a workspace `worker` with an agent below the chat.
async fn mixed_tree(
    manager: &Manager,
    repository_id: &str,
) -> (Workspace, Chat, Workspace, ProcessGroup) {
    let root = manager
        .create_workspace(repository_id, "root".into(), None, None, None)
        .await
        .unwrap();
    // Not this test's own process, whose children the other tests start.
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repository_id.to_owned().into(),
            "planning".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let (space, parent) = (chat.id.clone(), root.id.clone());
    manager
        .store
        .run(move |db| super::super::set_parent(db, &space, Some(&parent)))
        .await
        .unwrap();
    let worker = manager
        .create_workspace_for_chat(
            repository_id.to_owned().into(),
            "worker".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    for workspace in [&root, &worker] {
        manager
            .set_agent_state(&workspace.id, AgentState::Working, None, None)
            .await
            .unwrap();
    }
    (root, chat, worker, process)
}

/// Request the secret `secret` for `workspace` and route it, returning
/// its decider.
async fn decider(manager: &Manager, workspace: &Workspace, secret: &str) -> Option<String> {
    let requested = manager
        .request_approval(ApprovalRequest::new(
            &workspace.id,
            Target::Secret(secret.into()),
            "default",
            Specification::Secret(SecretSpecification {
                command: vec!["token-source".into()],
                tools: vec![],
            }),
            Lifetime::Workspace,
            Some("Publish the package"),
        ))
        .await
        .unwrap()
        .unwrap();
    manager.notify_approval(&workspace.name, &requested).await;
    let routed = manager.approval_request(requested.id).await.unwrap();
    routed.unwrap().decider
}

/// End `chat`'s agent, `process`, as a stop leaves it to resume.
async fn stop_chat(manager: &Manager, chat: &Chat, process: ProcessGroup) {
    process.kill();
    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE chats SET restore='resume' WHERE id=?1", [&id])?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_chat_below_a_workspace_decides_and_coordinates_for_the_spaces_below_it() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (root, chat, worker, process) = mixed_tree(&manager, &repo.id).await;
    let listed = manager.workspace(&worker.id).await.unwrap();
    let parent = listed.parent.unwrap();
    assert_eq!((parent.id, parent.kind), (chat.id.clone(), SpaceKind::Chat));
    let children = manager.workspace(&root.id).await.unwrap().children;
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].kind, SpaceKind::Chat);
    assert_eq!(
        manager.subtree(&root.id).await.unwrap(),
        [root.id.clone(), chat.id.clone(), worker.id.clone()]
    );

    // The nearest space above with an agent decides and coordinates.
    assert_eq!(
        decider(&manager, &worker, "first").await,
        Some(chat.id.clone())
    );
    manager
        .send_message(&worker.id, "first".into(), Vec::new(), &Attacher::User)
        .await
        .unwrap();
    let told = manager.chat_messages(&chat.id).await.unwrap();
    assert_eq!(told.last().unwrap().message, "worker: first");
    // The chat has an agent of its own, so none of the activity below it
    // reaches the workspace above.
    assert!(manager.delegated_spaces(&root.id).await.unwrap().is_empty());

    // Stopped, the chat is passed by on the way up, whatever its kind.
    stop_chat(&manager, &chat, process).await;
    assert_eq!(
        decider(&manager, &worker, "second").await,
        Some(root.id.clone())
    );
    manager
        .send_message(&worker.id, "second".into(), Vec::new(), &Attacher::User)
        .await
        .unwrap();
    let received = manager.agent_messages(&root.id).await.unwrap();
    assert_eq!(received.last().unwrap().message, "worker: second");
    let delegated = manager.delegated_spaces(&root.id).await.unwrap();
    assert_eq!(
        delegated.iter().map(|space| &space.id).collect::<Vec<_>>(),
        [&chat.id]
    );
}

#[tokio::test]
async fn removing_a_workspace_removes_a_chat_below_it_and_what_is_below_that() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (root, chat, worker, process) = mixed_tree(&manager, &repo.id).await;
    stop_chat(&manager, &chat, process).await;
    let check = manager.check_manual_removal(&root.id, false).await.unwrap();
    let below: Vec<_> = check.children.iter().map(|space| &space.id).collect();
    assert_eq!(below, [&worker.id, &chat.id], "deepest first");

    let removed = manager
        .remove_workspace(&root.id, UnsavedWork::default(), InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert_eq!(removed.children, ["worker", "planning"]);
    assert!(manager.recorded_chat(&chat.id).await.unwrap().is_none());
    for workspace in [&root, &worker] {
        let found = manager.find_workspace(&workspace.id).await.unwrap();
        assert!(found.is_none(), "{}", workspace.name);
    }
}

#[tokio::test]
async fn a_space_never_takes_one_above_it_as_its_child() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (root, chat, _, _process) = mixed_tree(&manager, &repo.id).await;
    let error = manager
        .assign_chat(&root.id, Some(chat.id.clone()))
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("planning is below the space it would take"),
        "{error:#}"
    );
    assert!(manager.workspace(&root.id).await.unwrap().parent.is_none());
}

#[tokio::test]
async fn a_chat_opens_workspaces_only_in_its_own_repository() {
    let (temp, manager) = manager().await;
    let mut repos = Vec::new();
    for name in ["repo", "other"] {
        let checkout = repository(temp.path(), name);
        repos.push(
            manager
                .register_repository(checkout.to_str().unwrap().into(), None, None)
                .await
                .unwrap(),
        );
    }
    let chat = manager
        .start_chat(
            repos[0].id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let error = manager
        .create_workspace_for_chat(
            repos[1].id.clone().into(),
            "foreign".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("planning is in another repository"),
        "{error:#}"
    );
    assert!(format!("{error:#}").contains("#1326"), "{error:#}");
    // Refused before anything was created.
    assert!(manager.workspace("foreign").await.is_err());
}
