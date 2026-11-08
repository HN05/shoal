//! Effective permissions in a tree of chats and workspaces.
use crate::{
    config::permissions::{Opening, OtherRepositories, PermissionSource, Pushes},
    daemon::{
        test_support::{chat, manager, repository, workspace_below as workspace},
        workspace::Manager,
    },
    model::Repository,
};

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

#[tokio::test]
async fn a_launch_takes_the_mode_it_starts_in_before_the_mode_is_recorded() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let config = "[modes.review.permissions]\npush = 'propose'\n\
                  [modes.chat.permissions]\npush = 'deny'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let (top, _process) = chat(&manager, "top", "chat", Some(&repo), None).await;
    let child = workspace(&manager, &repo, "child", &top.id, None).await;
    let launch = |mode: &'static str| {
        let (manager, child) = (manager.clone(), child.clone());
        async move { manager.launch_permissions(child, mode).await.unwrap().push }
    };
    let review = launch("review").await;
    assert_eq!(review.value, Pushes::Deny);
    assert_eq!(review.from, PermissionSource::Parent);
    let config = "[modes.review.permissions]\npush = 'propose'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    assert_eq!(launch("review").await.value, Pushes::Propose);
    assert_eq!(launch("worker").await.value, Pushes::Direct);
    // Nothing is recorded: the workspace has no mode of its own yet, so
    // its push is the key's default rather than the review mode's.
    let recorded = manager
        .space_permissions(&child.id, Some(&repo.id))
        .await
        .unwrap();
    assert_eq!(recorded.push.value, Pushes::Direct);
}
