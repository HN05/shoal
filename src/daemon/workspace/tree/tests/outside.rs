//! Trees below a chat without a repository, which takes children in every
//! repository and whose scope reaches every space.
use crate::{
    daemon::{
        chats::NewChat,
        test_support::{ProcessGroup, manager, repository},
        workspace::Manager,
        workspace_files::Attacher,
    },
    model::{Chat, Repository, Workspace},
    protocol::{
        AgentMethod, ItemContext, ItemMethod, NotificationMethod, QueueMethod, WorkspaceMethod,
    },
    state::AgentState,
};

/// Register a repository for each of `names`.
async fn repositories(
    temp: &tempfile::TempDir,
    manager: &Manager,
    names: &[&str],
) -> Vec<Repository> {
    let mut repos = Vec::new();
    for name in names {
        let checkout = repository(temp.path(), name);
        repos.push(
            manager
                .register_repository(checkout.to_str().unwrap().into(), None, None)
                .await
                .unwrap(),
        );
    }
    repos
}

/// Start the chat `name` without a repository, below `parent` when given,
/// running as the returned process.
async fn outside(manager: &Manager, name: &str, parent: Option<&str>) -> (Chat, ProcessGroup) {
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let new = NewChat {
        name: name.into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        pid: process.id(),
        origin: None,
        hands_over: false,
        parent: parent.map(str::to_owned),
    };
    let chat = manager.start_new_chat(None, None, new).await.unwrap();
    (chat, process)
}

/// Create the workspace `name` of `repository` below `parent` when given,
/// with an agent.
async fn worked(
    manager: &Manager,
    repository: &Repository,
    name: &str,
    parent: Option<&str>,
) -> Workspace {
    let workspace = manager
        .create_workspace_below(
            repository.id.clone().into(),
            name.into(),
            None,
            Default::default(),
            parent.map(str::to_owned),
        )
        .await
        .unwrap();
    manager
        .set_agent_state(&workspace.id, AgentState::Working, None, None)
        .await
        .unwrap();
    workspace
}

#[tokio::test]
async fn a_chat_without_a_repository_keeps_its_temporary_directory_in_root_dir() {
    let (temp, manager) = manager().await;
    let (master, _process) = outside(&manager, "master", None).await;
    assert_eq!(master.repository_id, None);
    let tmp_dir = master.tmp_dir.clone().unwrap();
    assert!(tmp_dir.is_dir(), "{}", tmp_dir.display());
    let root_dir = manager.config().root_dir(&manager.paths).unwrap();
    assert_eq!(tmp_dir, root_dir.join(".tmp").join(&master.id));
    let listed = manager.recorded_chat(&master.id).await.unwrap().unwrap();
    assert_eq!(listed.tmp_dir, Some(tmp_dir.clone()));
    // A daemon with another state directory sharing root_dir neither lists
    // nor takes it.
    let other =
        crate::tmp_dirs::Layout::outside_repositories(&root_dir, &temp.path().join("other"));
    assert_eq!(other.list().unwrap(), Vec::<String>::new());
    other.prepare("another").unwrap_err();
    manager.sweep_tmp_dirs().await.unwrap();
    assert!(tmp_dir.is_dir(), "{}", tmp_dir.display());
    // A new root_dir leaves it where the chat started, as a repository
    // keeps its directory.
    let moved = std::fs::canonicalize(temp.path()).unwrap().join("moved");
    crate::daemon::test_support::configure(&manager, &format!("[machine]\nroot_dir = {moved:?}\n"))
        .await;
    let listed = manager.recorded_chat(&master.id).await.unwrap().unwrap();
    assert_eq!(listed.tmp_dir, Some(tmp_dir.clone()));
    assert_eq!(listed.root_dir, Some(root_dir.clone()));
    let directory = manager.chat_checkout_path(&listed).await.unwrap();
    assert_eq!(directory, root_dir);
    let space = manager.chat_space(&listed).await.unwrap();
    assert_eq!(space.directories, std::slice::from_ref(&tmp_dir));
    manager.sweep_tmp_dirs().await.unwrap();
    assert!(tmp_dir.is_dir(), "{}", tmp_dir.display());
    manager.remove_chat(&master.id, None).await.unwrap();
    assert!(!tmp_dir.exists(), "{}", tmp_dir.display());
}

#[tokio::test]
async fn a_chat_without_a_repository_prepares_root_dir_as_registration_does() {
    let (temp, manager) = manager().await;
    let root = manager.prepare_chat_root().await.unwrap();
    assert!(root.is_dir(), "{}", root.display());
    // A repository registered afterwards keeps its directory there.
    let repos = repositories(&temp, &manager, &["repo"]).await;
    let directory = repos[0].workspaces_dir.clone().unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    assert!(directory.starts_with(&root), "{}", directory.display());
}

#[tokio::test]
async fn a_chat_without_a_repository_takes_children_in_every_repository() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    let (master, _process) = outside(&manager, "master", None).await;
    let first = worked(&manager, &repos[0], "first", Some(&master.id)).await;
    let second = worked(&manager, &repos[1], "second", Some(&master.id)).await;
    for workspace in [&first, &second] {
        let parent = manager.workspace(&workspace.id).await.unwrap().parent;
        assert_eq!(parent.map(|parent| parent.id), Some(master.id.clone()));
    }
    // A chat without a repository goes below another one.
    let (nested, _nested_process) = outside(&manager, "nested", Some(&master.id)).await;
    assert_eq!(nested.parent.map(|parent| parent.id), Some(master.id));
    // Every other parent keeps its children in its repository.
    let foreign = manager
        .create_workspace_below(
            repos[1].id.clone().into(),
            "foreign".into(),
            None,
            Default::default(),
            Some(first.id.clone()),
        )
        .await;
    assert!(foreign.is_err());
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let below_workspace = NewChat {
        name: "below".into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        pid: process.id(),
        origin: None,
        hands_over: false,
        parent: Some(first.id.clone()),
    };
    let refused = manager.start_new_chat(None, None, below_workspace).await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn activity_reaches_a_chat_without_a_repository_only_from_below_it() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    let (master, _process) = outside(&manager, "master", None).await;
    let below = worked(&manager, &repos[1], "below", Some(&master.id)).await;
    let user = worked(&manager, &repos[0], "user", None).await;
    assert_eq!(
        super::mixed::decider(&manager, &below, "first").await,
        Some(master.id.clone())
    );
    assert_eq!(super::mixed::decider(&manager, &user, "second").await, None);

    // Its hooks receive what the space below it sends, and acknowledge it.
    manager
        .send_message(&below.id, "done".into(), Vec::new(), &Attacher::User)
        .await
        .unwrap();
    let delivered = manager.space_updates(&master.id).await.unwrap();
    let last = delivered
        .messages
        .last()
        .map(|message| message.message.as_str());
    assert_eq!(last, Some("below: done"), "{delivered:?}");
    let deliveries = (delivered.updates.iter())
        .map(|update| update.delivery.clone())
        .collect();
    let messages = delivered
        .messages
        .iter()
        .map(|message| message.id)
        .collect();
    manager
        .acknowledge_space_delivery(&master.id, deliveries, messages, &[])
        .await
        .unwrap();
    let again = manager.space_updates(&master.id).await.unwrap();
    assert!(again.messages.is_empty(), "{again:?}");
}

#[tokio::test]
async fn removing_a_chat_without_a_repository_removes_its_children_in_every_repository() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    let (master, process) = outside(&manager, "master", None).await;
    let first = worked(&manager, &repos[0], "first", Some(&master.id)).await;
    let second = worked(&manager, &repos[1], "second", Some(&master.id)).await;
    super::mixed::stop_chat(&manager, &master, process).await;
    let removed = manager.remove_chat(&master.id, None).await.unwrap();
    let mut children: Vec<_> = removed.children.iter().map(String::as_str).collect();
    children.sort_unstable();
    assert_eq!(children, ["first", "second"]);
    assert!(manager.recorded_chat(&master.id).await.unwrap().is_none());
    for workspace in [&first, &second] {
        let found = manager.find_workspace(&workspace.id).await.unwrap();
        assert!(found.is_none(), "{}", workspace.name);
    }
}

#[tokio::test]
async fn a_broadcast_with_files_reaches_a_chat_without_a_repository() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo"]).await;
    let (_master, _process) = outside(&manager, "master", None).await;
    let talking = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let talk = manager
        .start_chat(
            repos[0].id.clone().into(),
            "talk".into(),
            "claude".into(),
            talking.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    let send = |attachments| {
        manager.send_all_message(
            None,
            "Note".into(),
            attachments,
            &crate::daemon::workspace_files::Attacher::User,
            None,
            None,
        )
    };
    let reached = send(vec![report.into()]).await.unwrap();
    assert_eq!(reached.chats, ["master".to_owned(), talk.name.clone()]);
    let reached = send(Vec::new()).await.unwrap();
    assert_eq!(reached.chats, ["master".to_owned(), talk.name]);
}

#[tokio::test]
async fn a_chat_without_a_repository_copies_a_workspace_of_any_repository() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    let (master, _process) = outside(&manager, "master", None).await;
    let user = worked(&manager, &repos[1], "user", None).await;
    let caller = crate::daemon::scope::Caller {
        space_id: master.id.clone(),
        execution: None,
    };
    let mut source = crate::protocol::CopySource {
        workspace: user.name.clone(),
        checkpoint: None,
    };
    manager.copy_within(&caller, &mut source).await.unwrap();
    assert_eq!(source.workspace, user.id);
}

#[tokio::test]
async fn the_scope_of_a_chat_without_a_repository_reaches_every_space_it_may_change() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    let (master, _process) = outside(&manager, "master", None).await;
    let (below, user) = (
        worked(&manager, &repos[0], "below", Some(&master.id)).await,
        worked(&manager, &repos[1], "user", None).await,
    );
    let below_id = below.id.clone();
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repos[0].id.clone().into(),
            "planning".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let message = || AgentMethod::SendAgentMessage {
        space: "user".into(),
        message: "Rebase onto main".into(),
        attachments: Vec::new(),
        sender_chat: None,
    };
    let token = manager.chat_scope(&master.id).await.unwrap();
    let mut method = message();
    crate::daemon::scope::authorize(&manager, Some(&token), &mut method)
        .await
        .unwrap();
    let AgentMethod::SendAgentMessage { space, .. } = method else {
        unreachable!("the method stays a message");
    };
    assert_eq!(space, user.id);
    // A chat of a repository reaches only its own tree.
    let scoped = manager.chat_scope(&chat.id).await.unwrap();
    let refused = crate::daemon::scope::authorize(&manager, Some(&scoped), &mut message()).await;
    assert!(refused.is_err());

    // It lists every space and repository.
    let caller = manager.caller(&token).await.unwrap().unwrap();
    let listed = manager.visible_chats(Some(&caller)).await.unwrap();
    assert_eq!(listed.len(), 2);
    let visible = manager.visible_repositories(Some(&caller)).await.unwrap();
    assert_eq!(visible.len(), 2);
    let mut workspaces = WorkspaceMethod::ListWorkspaces;
    crate::daemon::scope::authorize(&manager, Some(&token), &mut workspaces)
        .await
        .unwrap();

    // It reads the items of any registered repository from its checkout, as
    // its agent account, and changes them as its own space acting there.
    let details = |repository: &Repository| ItemMethod::IssueDetails {
        context: ItemContext::Checkout(repository.id.clone()),
        item: "7".into(),
    };
    crate::daemon::scope::authorize(&manager, Some(&token), &mut details(&repos[1]))
        .await
        .unwrap();
    let mut own = details(&repos[0]);
    let reading = crate::daemon::scope::authorize(&manager, Some(&scoped), &mut own).await;
    assert!(reading.is_err());
    let mut close = ItemMethod::ItemAction {
        context: ItemContext::Checkout(repos[1].id.clone()),
        kind: crate::forge::items::link::ItemKind::Issue,
        item: Some("7".into()),
        action: crate::forge::items::action::Action::Close,
    };
    crate::daemon::scope::authorize(&manager, Some(&token), &mut close)
        .await
        .unwrap();
    let ItemMethod::ItemAction { context, .. } = close else {
        unreachable!("the method stays an item action");
    };
    let ItemContext::ChatIn {
        chat: acting,
        repository,
    } = context
    else {
        panic!("expected the chat acting in the checkout: {context:?}");
    };
    assert_eq!(
        (acting, repository),
        (master.id.clone(), repos[1].id.clone())
    );

    // It resumes the agent of a space it reaches only when it or a space
    // below it stopped that agent.
    let resume = |stopper: &str| {
        let last: crate::execution::recovery::LastSession = serde_json::from_value(
            serde_json::json!({"execution": "e1", "agent": "claude", "stopped_by": stopper}),
        )
        .unwrap();
        last.write(&manager.paths, &user.id).unwrap();
        QueueMethod::QueueRestore {
            workspace: user.id.clone(),
            execution: "e1".into(),
            env: Default::default(),
            now: false,
        }
    };
    let by_other =
        crate::daemon::scope::authorize(&manager, Some(&token), &mut resume(&chat.id)).await;
    assert!(by_other.is_err());
    let mut by_below = resume(&below_id);
    crate::daemon::scope::authorize(&manager, Some(&token), &mut by_below)
        .await
        .unwrap();

    // What the scope keeps from every agent stays the user's.
    let mut merge = ItemMethod::ItemAction {
        context: ItemContext::Chat(master.id.clone()),
        kind: crate::forge::items::link::ItemKind::Pr,
        item: Some("1".into()),
        action: crate::forge::items::action::Action::CancelAutoMerge,
    };
    let merging = crate::daemon::scope::authorize(&manager, Some(&token), &mut merge).await;
    assert!(merging.is_err());
    let mut notifications = NotificationMethod::ListNotifications {
        unread_only: true,
        limit: 10,
    };
    let reading = crate::daemon::scope::authorize(&manager, Some(&token), &mut notifications).await;
    assert!(reading.is_err());
}
