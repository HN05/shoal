//! Effective permissions in a tree of chats and workspaces.
use crate::{
    config::permissions::{Opening, OtherRepositories, PermissionSource, Pushes},
    daemon::{
        chats::NewChat,
        test_support::manager,
        workspace::{Manager, NewWorkspace},
    },
    model::{Chat, Repository, Workspace},
    test_support::{ProcessGroup, repository},
};

/// Start the chat `name` in `mode`, of `repository` or without one, below
/// `parent`, running as the returned process.
async fn chat(
    manager: &Manager,
    name: &str,
    mode: &str,
    repository: Option<&Repository>,
    parent: Option<&str>,
) -> (Chat, ProcessGroup) {
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let new = NewChat {
        name: name.into(),
        agent: "claude".into(),
        mode: mode.into(),
        pid: process.id(),
        origin: None,
        hands_over: false,
        parent: parent.map(str::to_owned),
    };
    let repository = repository.map(|repository| repository.id.clone().into());
    let chat = manager.start_new_chat(repository, None, new).await.unwrap();
    (chat, process)
}

/// Create the workspace `name` of `repository` below `parent`, recording
/// `mode` as its agent's.
async fn workspace(
    manager: &Manager,
    repository: &Repository,
    name: &str,
    parent: &str,
    mode: Option<&str>,
) -> Workspace {
    let new = NewWorkspace {
        parent: Some(parent.to_owned()),
        ..Default::default()
    };
    let workspace = manager
        .create_workspace_with(&repository.id, name.into(), None, new)
        .await
        .unwrap();
    let (id, mode) = (workspace.id.clone(), mode.map(str::to_owned));
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET mode=?2 WHERE id=?1",
                rusqlite::params![id, mode],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    workspace
}

async fn register(temp: &tempfile::TempDir, manager: &Manager, name: &str) -> Repository {
    let checkout = repository(temp.path(), name);
    manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_space_never_has_more_than_the_space_above_it() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let (top, _process) = chat(&manager, "top", "chat", Some(&repo), None).await;
    let worker = workspace(&manager, &repo, "worker", &top.id, Some("worker")).await;
    let split = workspace(&manager, &repo, "split", &worker.id, None).await;
    let of = |id: String| {
        let manager = manager.clone();
        let repository = repo.id.clone();
        async move {
            (manager.space_permissions(&id, Some(&repository)))
                .await
                .unwrap()
        }
    };

    // A top-level chat merges directly; the worker below it keeps its own.
    let chat = of(top.id.clone()).await;
    assert_eq!(chat.merge.value, Opening::Direct);
    let below = of(worker.id.clone()).await;
    assert_eq!(below.merge.value, Opening::Propose);
    assert_eq!(below.merge.from, PermissionSource::Mode);
    assert_eq!(
        below.issue_other_repositories.value,
        OtherRepositories::Propose
    );

    // A config change narrowing the chat reaches every space below it.
    let config = "[modes.chat.permissions]\npush = 'propose'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    for id in [worker.id.clone(), split.id.clone()] {
        let push = of(id).await.push;
        assert_eq!(push.value, Pushes::Propose);
        assert_eq!(push.from, PermissionSource::Parent);
    }
    // A wider mode below a narrower one is narrowed, a narrower one kept.
    let config = "[modes.chat.permissions]\npush = 'propose'\n\
                  [modes.worker.permissions]\npush = 'deny'\nmerge = 'direct'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let below = of(worker.id.clone()).await;
    assert_eq!(below.push.value, Pushes::Deny);
    assert_eq!(below.push.from, PermissionSource::Mode);
    assert_eq!(below.merge.value, Opening::Direct);
    let split = of(split.id.clone()).await;
    assert_eq!(split.push.value, Pushes::Deny);
    assert_eq!(split.push.from, PermissionSource::Parent);
    // Without a mode of its own, the split has [permissions]'s merge.
    assert_eq!(split.merge.value, Opening::Propose);
}

#[tokio::test]
async fn a_chat_without_a_repository_resolves_in_the_repository_of_the_action() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let other = register(&temp, &manager, "other").await;
    let config = "[modes.orchestrate.permissions]\nmerge = 'propose'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let (master, _process) = chat(&manager, "master", "orchestrate", None, None).await;
    let (child, _child) = chat(&manager, "child", "chat", Some(&repo), Some(&master.id)).await;
    let merge = |id: String, repository: Option<String>| {
        let manager = manager.clone();
        async move {
            let permissions = manager.space_permissions(&id, repository.as_deref());
            permissions.await.unwrap().merge
        }
    };
    assert_eq!(
        merge(master.id.clone(), Some(repo.id.clone())).await.value,
        Opening::Propose
    );
    assert_eq!(
        merge(master.id.clone(), Some(other.id.clone())).await.value,
        Opening::Direct
    );
    // Without a repository, the global config applies.
    assert_eq!(merge(master.id.clone(), None).await.value, Opening::Direct);
    // The chat below resolves the master's mode in its own repository.
    let below = merge(child.id.clone(), Some(repo.id.clone())).await;
    assert_eq!(below.value, Opening::Propose);
    assert_eq!(below.from, PermissionSource::Parent);
}
