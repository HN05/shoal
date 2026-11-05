//! Trees below a chat without a repository, which takes children in every
//! repository and whose scope reaches every space.
use crate::{
    daemon::{chats::NewChat, workspace::Manager},
    model::{Chat, Repository, Workspace},
    state::AgentState,
    test_support::{ProcessGroup, manager, repository},
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
    let (_temp, manager) = manager().await;
    let (master, _process) = outside(&manager, "master", None).await;
    assert_eq!(master.repository_id, None);
    let tmp_dir = master.tmp_dir.clone().unwrap();
    assert!(tmp_dir.is_dir(), "{}", tmp_dir.display());
    let root = manager.config().root_dir(&manager.paths).unwrap();
    assert!(tmp_dir.starts_with(root), "{}", tmp_dir.display());
    let listed = manager.recorded_chat(&master.id).await.unwrap().unwrap();
    assert_eq!(listed.tmp_dir, Some(tmp_dir.clone()));
    manager.remove_chat(&master.id, None).await.unwrap();
    assert!(!tmp_dir.exists(), "{}", tmp_dir.display());
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
