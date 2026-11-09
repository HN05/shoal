//! Effective permissions in a tree of chats and workspaces.
use crate::client::repository_ids::RepositoryId;
use crate::{
    client::{space_ids::SpaceId, space_names::SpaceSelector},
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
    let of = |id: SpaceId| {
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
    let merge = |id: SpaceId, repository: Option<RepositoryId>| {
        let manager = manager.clone();
        async move {
            let permissions = manager.space_permissions(&id, repository.as_ref());
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
        async move {
            let permissions = manager.launch_permissions(child, mode, Default::default());
            permissions.await.unwrap().push
        }
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

/// Record `assignments` as the `--permission` given to the agent of the
/// space `id`, in `table`.
async fn give(manager: &Manager, table: &str, id: &SpaceId, assignments: &[&str]) {
    let assignments: Vec<String> = assignments.iter().map(|a| (*a).to_owned()).collect();
    let narrowing = crate::config::permissions::PermissionsConfig::assigned(&assignments).unwrap();
    let json = serde_json::to_string(&narrowing).unwrap();
    let (sql, id) = (
        format!("UPDATE {table} SET narrowing=?2 WHERE id=?1"),
        id.clone(),
    );
    manager
        .store
        .run(move |db| Ok(db.execute(&sql, rusqlite::params![id, json])?))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_given_permission_applies_to_its_space_and_caps_the_spaces_below() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let (top, _process) = chat(&manager, "top", "chat", Some(&repo), None).await;
    let worker = workspace(&manager, &repo, "worker", &top.id, Some("worker")).await;
    let split = workspace(&manager, &repo, "split", &worker.id, None).await;
    give(&manager, "chats", &top.id, &["merge=propose"]).await;
    // Wider than the worker mode's merge and the chat's above it.
    let assignments = ["push=propose", "merge=direct"];
    give(&manager, "workspaces", &worker.id, &assignments).await;
    let of = |id: SpaceId| {
        let manager = manager.clone();
        let repository = repo.id.clone();
        async move {
            let permissions = manager.space_permissions(&id, Some(&repository));
            permissions.await.unwrap()
        }
    };
    let chat = of(top.id.clone()).await;
    assert_eq!(chat.merge.value, Opening::Propose);
    assert_eq!(chat.merge.from, PermissionSource::Given);
    let below = of(worker.id.clone()).await;
    assert_eq!(below.push.value, Pushes::Propose);
    assert_eq!(below.push.from, PermissionSource::Given);
    assert_eq!(below.merge.value, Opening::Direct);
    assert_eq!(below.merge.from, PermissionSource::Given);
    let split = of(split.id.clone()).await;
    assert_eq!(split.push.value, Pushes::Propose);
    assert_eq!(split.push.from, PermissionSource::Parent);
}

#[tokio::test]
async fn a_scoped_process_gives_only_what_it_holds_or_was_recorded() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let worker = (manager.create_workspace(&repo.id, "worker".into(), None, None, None)).await;
    let worker = worker.unwrap();
    let child = workspace(&manager, &repo, "child", &worker.id, Some("worker")).await;
    let (talk, _process) = chat(&manager, "talk", "chat", Some(&repo), Some(&worker.id)).await;
    // The user gave both a merge the worker does not hold.
    give(&manager, "workspaces", &child.id, &["merge=direct"]).await;
    give(&manager, "chats", &talk.id, &["merge=direct"]).await;
    let talk = manager.chat_by_id(&talk.id).await.unwrap().unwrap();
    let assigned = |assignments: &[&str]| {
        let assignments: Vec<String> = assignments.iter().map(|a| (*a).to_owned()).collect();
        crate::config::permissions::PermissionsConfig::assigned(&assignments).unwrap()
    };
    let (giver, child) = (Some(&worker.id), SpaceSelector::id(&child.id));

    // What the space recorded comes back, with what the giver holds.
    let recorded = assigned(&["merge=direct", "push=propose"]);
    let registered = manager.ensure_workspace_agent_held(giver, &child, "worker", &recorded);
    registered.await.unwrap();
    manager
        .ensure_restore_held(giver, &talk, &recorded)
        .await
        .unwrap();
    // A value the giver does not hold, recorded nowhere, is refused.
    let wider = assigned(&["merge=direct", "api_writes=direct"]);
    let registered = manager.ensure_workspace_agent_held(giver, &child, "worker", &wider);
    assert!(registered.await.is_err());
    assert!(
        manager
            .ensure_restore_held(giver, &talk, &wider)
            .await
            .is_err()
    );
    // A restart takes nothing as recorded.
    let change = crate::config::permissions::PermissionChange {
        set: recorded,
        dropped: Vec::new(),
    };
    let restarted = manager.ensure_chat_restart_held(giver, Some(&talk), &change);
    assert!(restarted.await.is_err());
    // The user gives any.
    let registered = manager.ensure_workspace_agent_held(None, &child, "worker", &wider);
    registered.await.unwrap();
    manager
        .ensure_restore_held(None, &talk, &wider)
        .await
        .unwrap();
    let restarted = manager.ensure_chat_restart_held(None, Some(&talk), &change);
    restarted.await.unwrap();
}

#[tokio::test]
async fn a_scoped_process_drops_a_key_only_to_a_value_it_holds() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let (top, _process) = chat(&manager, "top", "chat", Some(&repo), None).await;
    let parent = workspace(&manager, &repo, "parent", &top.id, Some("worker")).await;
    let child = workspace(&manager, &repo, "child", &parent.id, Some("worker")).await;
    // The user gave the chat less push than the spaces below it.
    give(&manager, "chats", &top.id, &["push=propose"]).await;
    give(&manager, "workspaces", &parent.id, &["push=direct"]).await;
    give(&manager, "workspaces", &child.id, &["push=propose"]).await;
    let child = manager.workspace_by_id(&child.id).await.unwrap();
    let space = crate::model::PresentSpace::Workspace(child);
    let dropped = |assignment: &str| {
        crate::config::permissions::PermissionChange::assigned(assignment).unwrap()
    };
    let check = |giver: Option<SpaceId>, change| {
        let (manager, space, repository) = (manager.clone(), space.clone(), repo.id.clone());
        async move {
            let held =
                manager.ensure_change_held(giver.as_ref(), &space, &change, Some(&repository));
            held.await
        }
    };
    // Without its push, the child would push directly, as its parent does,
    // which the chat does not hold.
    let refused = check(Some(top.id.clone()), dropped("push=mode")).await;
    assert!(format!("{:#}", refused.unwrap_err()).contains("push=mode would give push=direct"));
    // A key the chat holds as much of drops, and the user drops any.
    check(Some(top.id.clone()), dropped("merge=mode"))
        .await
        .unwrap();
    check(None, dropped("push=mode")).await.unwrap();

    // A queued or registered agent or a restore that leaves a recorded key
    // out drops it.
    let (talk, _talk) = chat(&manager, "talk", "chat", Some(&repo), Some(&parent.id)).await;
    give(&manager, "chats", &talk.id, &["push=propose"]).await;
    let talk = manager.chat_by_id(&talk.id).await.unwrap().unwrap();
    let none = crate::config::permissions::PermissionsConfig::default();
    let (giver, child) = (Some(&top.id), SpaceSelector::id(space.id()));
    let registered = manager.ensure_workspace_agent_held(giver, &child, "worker", &none);
    assert!(registered.await.is_err());
    assert!(
        manager
            .ensure_restore_held(giver, &talk, &none)
            .await
            .is_err()
    );
    let recorded = talk.narrowing;
    let registered = manager.ensure_workspace_agent_held(giver, &child, "worker", &recorded);
    registered.await.unwrap();
    manager
        .ensure_restore_held(giver, &talk, &recorded)
        .await
        .unwrap();

    // A dropped key falls back in the mode of the agent that drops it.
    let config = "[modes.review.permissions]\npush = 'deny'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let quiet = workspace(&manager, &repo, "quiet", &parent.id, Some("review")).await;
    give(&manager, "workspaces", &quiet.id, &["push=deny"]).await;
    let quiet = SpaceSelector::id(&quiet.id);
    let registered = manager.ensure_workspace_agent_held(giver, &quiet, "review", &none);
    registered.await.unwrap();
    let registered = manager.ensure_workspace_agent_held(giver, &quiet, "worker", &none);
    assert!(registered.await.is_err());
}
