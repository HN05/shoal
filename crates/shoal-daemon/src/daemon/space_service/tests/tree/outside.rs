//! Trees below a chat without a repository, which takes children in every
//! repository and whose scope reaches every space.
use crate::execution::recovery::LastSessionExt;
use crate::removal::{InspectionPolicy, UnsavedWork};
use crate::{
    daemon::{
        chats::NewChat,
        manager::Manager,
        test_support::{ProcessGroup, agent_process, manager, repository},
    },
    model::{Chat, Repository, Workspace},
    protocol::{AgentMethod, AlertMethod, AlertTarget, ItemContext, ItemMethod, SpaceMethod},
    state::AgentState,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

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
                .repositories
                .register(checkout.to_str().unwrap().into(), None, None)
                .await
                .unwrap(),
        );
    }
    repos
}

/// Start the chat `name` without a repository, below `parent` when given,
/// running as the returned process.
async fn outside(
    manager: &std::sync::Arc<Manager>,
    name: &str,
    parent: Option<&SpaceId>,
) -> (Chat, ProcessGroup) {
    let new = NewChat {
        priority: None,
        narrowing: Default::default(),
        name: name.into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        origin: None,
        parent: parent.cloned(),
    };
    let chat = manager
        .spaces
        .start_new_chat(&**manager, None, None, new)
        .await
        .unwrap();
    let chat = manager.run_chat_agent(chat, None).await.unwrap();
    let process = agent_process(manager, &chat.id).await;
    (chat, process)
}

/// Create the workspace `name` of `repository` below `parent` when given,
/// with an agent.
async fn worked(
    manager: &Manager,
    repository: &Repository,
    name: &str,
    parent: Option<&SpaceId>,
) -> Workspace {
    let workspace = manager
        .spaces
        .create_workspace_below(
            manager,
            repository.id.clone().into(),
            name.into(),
            None,
            Default::default(),
            parent.cloned(),
        )
        .await
        .unwrap();
    manager
        .agents
        .set_agent_state(
            manager,
            &SpaceSelector::id(&workspace.id),
            AgentState::Working,
            None,
            None,
        )
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
    let listed = manager
        .spaces
        .chat_by_id(&master.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listed.tmp_dir, Some(tmp_dir.clone()));
    // A daemon with another state directory sharing root_dir neither lists
    // nor takes it.
    let other =
        crate::tmp_dirs::Layout::outside_repositories(&root_dir, &temp.path().join("other"));
    assert_eq!(other.list().unwrap(), Vec::<String>::new());
    other.prepare(&SpaceId::from("another")).unwrap_err();
    manager.spaces.sweep_tmp_dirs(&*manager).await.unwrap();
    assert!(tmp_dir.is_dir(), "{}", tmp_dir.display());
    // A new root_dir leaves it where the chat started, as a repository
    // keeps its directory.
    let moved = std::fs::canonicalize(temp.path()).unwrap().join("moved");
    crate::daemon::test_support::configure(&manager, &format!("[machine]\nroot_dir = {moved:?}\n"))
        .await;
    let listed = manager
        .spaces
        .chat_by_id(&master.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listed.tmp_dir, Some(tmp_dir.clone()));
    assert_eq!(listed.workdir.root_dir(), Some(root_dir.as_path()));
    let directory = manager
        .spaces
        .chat_checkout_path(&*manager, &listed)
        .await
        .unwrap();
    assert_eq!(directory, root_dir);
    let space = crate::model::Space::from(listed);
    let space = (manager
        .spaces
        .space_ties(
            &*manager,
            &space,
            crate::daemon::space_processes::WorktreeTies::None,
        )
        .await)
        .unwrap();
    assert_eq!(space.directories, std::slice::from_ref(&tmp_dir));
    manager.spaces.sweep_tmp_dirs(&*manager).await.unwrap();
    assert!(tmp_dir.is_dir(), "{}", tmp_dir.display());
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&master.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!tmp_dir.exists(), "{}", tmp_dir.display());
}

#[tokio::test]
async fn a_chat_without_a_repository_prepares_root_dir_as_registration_does() {
    let (temp, manager) = manager().await;
    let root = manager.repositories.prepare_chat_root().await.unwrap();
    assert!(root.is_dir(), "{}", root.display());
    // A repository registered afterwards keeps its directory there.
    let repos = repositories(&temp, &manager, &["repo"]).await;
    let directory = repos[0].workspaces_dir.clone();
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
        let parent = manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .parent;
        assert_eq!(parent.map(|parent| parent.id), Some(master.id.clone()));
    }
    // A chat without a repository goes below another one.
    let (nested, _nested_process) = outside(&manager, "nested", Some(&master.id)).await;
    assert_eq!(nested.parent.map(|parent| parent.id), Some(master.id));
    // Every other parent keeps its children in its repository.
    let foreign = manager
        .spaces
        .create_workspace_below(
            &*manager,
            repos[1].id.clone().into(),
            "foreign".into(),
            None,
            Default::default(),
            Some(first.id.clone()),
        )
        .await;
    assert!(foreign.is_err());
    let below_workspace = NewChat {
        priority: None,
        narrowing: Default::default(),
        name: "below".into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        origin: None,
        parent: Some(first.id.clone()),
    };
    let refused = manager
        .spaces
        .start_new_chat(&*manager, None, None, below_workspace)
        .await;
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
        .coordination
        .send_alert(
            &*manager,
            Some(&crate::daemon::test_support::caller_in(&SpaceSelector::id(
                &below.id,
            ))),
            crate::protocol::AlertTarget::Up,
            "done".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    let delivered = manager
        .links
        .agent_updates(&*manager, &SpaceSelector::id(&master.id))
        .await
        .unwrap();
    let last = delivered.alerts.last().map(|alert| alert.text());
    let done = format!("{} below: done", below.number);
    assert_eq!(last, Some(done), "{delivered:?}");
    let deliveries = (delivered.updates.iter())
        .map(|update| update.delivery.clone())
        .collect();
    let messages = delivered.alerts.iter().map(|message| message.id).collect();
    let master_selector = SpaceSelector::id(&master.id);
    (manager.links)
        .acknowledge_pr_updates(&*manager, &master_selector, deliveries)
        .await
        .unwrap();
    (manager.agents)
        .mark_agent_messages_delivered(&*manager, &master_selector, messages)
        .await
        .unwrap();
    let again = manager
        .links
        .agent_updates(&*manager, &SpaceSelector::id(&master.id))
        .await
        .unwrap();
    assert!(again.alerts.is_empty(), "{again:?}");
}

#[tokio::test]
async fn removing_a_chat_without_a_repository_removes_its_children_in_every_repository() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    let (master, process) = outside(&manager, "master", None).await;
    let first = worked(&manager, &repos[0], "first", Some(&master.id)).await;
    let second = worked(&manager, &repos[1], "second", Some(&master.id)).await;
    super::mixed::stop_chat(&manager, &master, process).await;
    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&master.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let mut children: Vec<_> = removed.children.iter().map(SpaceName::as_str).collect();
    children.sort_unstable();
    assert_eq!(children, ["first", "second"]);
    assert!(
        manager
            .spaces
            .chat_by_id(&master.id)
            .await
            .unwrap()
            .is_none()
    );
    for workspace in [&first, &second] {
        let found = manager.spaces.find_workspace(&workspace.id).await.unwrap();
        assert!(found.is_none(), "{}", workspace.name);
    }
}

#[tokio::test]
async fn a_broadcast_with_files_reaches_a_chat_without_a_repository() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo"]).await;
    let (_master, _process) = outside(&manager, "master", None).await;
    let talk = manager
        .start_chat(
            repos[0].id.clone().into(),
            "talk".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    let send = |attachments| {
        manager
            .coordination
            .send_all_message(&*manager, None, None, "Note".into(), attachments)
    };
    let reached = send(vec![report.into()]).await.unwrap();
    let names = crate::daemon::test_support::names;
    assert_eq!(
        names(&reached.reached.now.chats),
        ["master", talk.name.as_str()]
    );
    let reached = send(Vec::new()).await.unwrap();
    assert_eq!(
        names(&reached.reached.now.chats),
        ["master", talk.name.as_str()]
    );
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
        granted: None,
        read_only: false,
        vouched: None,
    };
    let mut source = crate::protocol::CopySource {
        workspace: SpaceSelector::id(&user.id),
        checkpoint: None,
    };
    manager
        .coordination
        .copy_within(&*manager, &caller, &mut source)
        .await
        .unwrap();
    assert_eq!(source.workspace, SpaceSelector::id(&user.id));
}

#[tokio::test]
async fn the_scope_of_a_chat_without_a_repository_reaches_every_space_it_may_change() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    let (master, _process) = outside(&manager, "master", None).await;
    let user = worked(&manager, &repos[1], "user", None).await;
    let chat = manager
        .start_chat(
            repos[0].id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let message = || AlertMethod::SendAlert {
        from: None,
        to: AlertTarget::Space {
            space: SpaceSelector::id(&user.id),
        },
        message: "Rebase onto main".into(),
        attachments: Vec::new(),
    };
    let token = manager.spaces.chat_scope(&master.id).await.unwrap();
    let mut method = message();
    crate::daemon::scope::authorize(&manager, Some(&token), &mut method)
        .await
        .unwrap();
    let AlertMethod::SendAlert {
        from: None,
        to: AlertTarget::Space { space },
        ..
    } = method
    else {
        unreachable!("the method stays an alert to the space");
    };
    assert_eq!(space, SpaceSelector::id(&user.id));
    // A chat of a repository reaches only its own tree.
    let scoped = manager.spaces.chat_scope(&chat.id).await.unwrap();
    let refused = crate::daemon::scope::authorize(&manager, Some(&scoped), &mut message()).await;
    assert!(refused.is_err());

    // It lists every space and repository.
    let caller = manager
        .coordination
        .caller(&*manager, &token)
        .await
        .unwrap()
        .unwrap();
    let listed = manager
        .coordination
        .visible_spaces(&*manager, Some(&caller))
        .await
        .unwrap();
    assert_eq!(listed.len(), 3);
    let visible = manager
        .coordination
        .visible_repositories(&*manager, Some(&caller))
        .await
        .unwrap();
    assert_eq!(visible.len(), 2);
    let mut workspaces = SpaceMethod::ListSpaces;
    crate::daemon::scope::authorize(&manager, Some(&token), &mut workspaces)
        .await
        .unwrap();

    // It reads the items of any registered repository from its checkout, as
    // its agent account, and changes them as its own space acting there.
    let details = |repository: &Repository| ItemMethod::IssueDetails {
        context: ItemContext::Checkout(repository.id.clone()),
        item: "7".parse().unwrap(),
        closed: false,
    };
    crate::daemon::scope::authorize(&manager, Some(&token), &mut details(&repos[1]))
        .await
        .unwrap();
    let mut own = details(&repos[0]);
    let reading = crate::daemon::scope::authorize(&manager, Some(&scoped), &mut own).await;
    assert!(reading.is_err());
    let mut close = ItemMethod::ItemAction {
        target: ItemContext::Checkout(repos[1].id.clone()).into(),
        kind: crate::forge::items::link::ItemKind::Issue,
        item: Some("7".parse().unwrap()),
        action: crate::forge::items::action::Action::Close,
        confirmed: None,
    };
    crate::daemon::scope::authorize(&manager, Some(&token), &mut close)
        .await
        .unwrap();
    let ItemMethod::ItemAction { target, .. } = close else {
        unreachable!("the method stays an item action");
    };
    let Some(ItemContext::Space {
        space: acting,
        repository: Some(repository),
    }) = target.context
    else {
        panic!("expected the chat acting in the checkout: {target:?}");
    };
    assert_eq!(
        (acting, repository),
        (SpaceSelector::id(&master.id), repos[1].id.clone())
    );

    // It resumes the agent of any space it reaches.
    let last: crate::execution::recovery::LastSession = serde_json::from_value(
        serde_json::json!({"execution": "e1", "agent": "claude", "mode": "worker"}),
    )
    .unwrap();
    last.write(&manager.paths, &user.id).unwrap();
    let mut resume = AgentMethod::ResumeAgent {
        space: SpaceSelector::id(&user.id),
        resume: crate::model::AgentResume {
            executions: ["e1".into()].into(),
            ..Default::default()
        },
    };
    crate::daemon::scope::authorize(&manager, Some(&token), &mut resume)
        .await
        .unwrap();

    // What the scope keeps from every agent stays the user's.
    let mut merge = ItemMethod::ItemAction {
        target: ItemContext::space(SpaceSelector::id(&master.id)).into(),
        kind: crate::forge::items::link::ItemKind::Pr,
        item: Some("1".parse().unwrap()),
        action: crate::forge::items::action::Action::CancelAutoMerge,
        confirmed: None,
    };
    let merging = crate::daemon::scope::authorize(&manager, Some(&token), &mut merge).await;
    assert!(merging.is_err());
    // A master session reads the user's notifications.
    let mut notifications = AlertMethod::ListAlerts {
        to: crate::protocol::AlertsFor::User,
        unread_only: true,
        limit: 10,
    };
    let reading = crate::daemon::scope::authorize(&manager, Some(&token), &mut notifications).await;
    reading.unwrap();
}
