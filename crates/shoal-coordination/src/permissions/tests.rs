//! Effective permissions in a tree of chats and workspaces.
use std::sync::Arc;

use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    config::permissions::{Opening, OtherRepositories, PermissionSource, Pushes},
    model::{Space, Workspace},
    scope::Caller,
    stand_in::Fixture,
};

async fn chat(
    fixture: &Fixture,
    name: &str,
    mode: &str,
    repository: Option<&RepositoryId>,
    parent: Option<&SpaceId>,
) -> Space {
    fixture.chat(name, mode, repository, parent).await
}

async fn workspace(
    fixture: &Fixture,
    repository: &RepositoryId,
    name: &str,
    parent: &SpaceId,
    mode: Option<String>,
) -> Workspace {
    let repository = repository.as_str();
    (fixture.workspace_in(repository, name, Some(parent), mode.as_deref())).await
}

#[tokio::test]
async fn a_space_never_has_more_than_the_space_above_it() {
    let fixture = Arc::new(Fixture::new().await);
    let repo = fixture.repository("repo").await;
    let top = chat(&fixture, "top", "chat", Some(&repo), None).await;
    let worker = workspace(&fixture, &repo, "worker", &top.id, Some("worker".into())).await;
    let split = workspace(&fixture, &repo, "split", &worker.id, None).await;
    let of = |id: SpaceId| {
        let fixture = fixture.clone();
        let repository = repo.clone();
        async move {
            (fixture
                .coordination
                .space_permissions(&**fixture, &id, Some(&repository)))
            .await
            .unwrap()
        }
    };

    // A top-level chat opens issues elsewhere directly; the worker below it
    // keeps its own.
    let chat = of(top.id.clone()).await;
    assert_eq!(
        chat.issue_other_repositories.value,
        OtherRepositories::Direct
    );
    let below = of(worker.id.clone()).await;
    assert_eq!(below.merge.value, Opening::Propose);
    assert_eq!(below.merge.from, PermissionSource::Mode);
    assert_eq!(
        below.issue_other_repositories.value,
        OtherRepositories::Propose
    );

    // A config change narrowing the chat reaches every space below it.
    let config = "[modes.chat.permissions]\npush = 'propose'\n";
    fixture.set_local_config(&repo, config);
    for id in [worker.id.clone(), split.id.clone()] {
        let push = of(id).await.push;
        assert_eq!(push.value, Pushes::Propose);
        assert_eq!(push.from, PermissionSource::Parent);
    }
    // A wider mode below a narrower one is narrowed, a narrower one kept.
    let config = "[modes.chat.permissions]\npush = 'propose'\nmerge = 'direct'\n\
                  [modes.worker.permissions]\npush = 'deny'\nmerge = 'direct'\n";
    fixture.set_local_config(&repo, config);
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
    let fixture = Arc::new(Fixture::new().await);
    let repo = fixture.repository("repo").await;
    let other = fixture.repository("other").await;
    let config = "[modes.orchestrate.permissions]\nmerge = 'propose'\n\
                  [modes.chat.permissions]\nmerge = 'direct'\n";
    fixture.set_local_config(&repo, config);
    let master = chat(&fixture, "master", "orchestrate", None, None).await;
    let child = chat(&fixture, "child", "chat", Some(&repo), Some(&master.id)).await;
    let merge = |id: SpaceId, repository: Option<RepositoryId>| {
        let fixture = fixture.clone();
        async move {
            let permissions =
                fixture
                    .coordination
                    .space_permissions(&**fixture, &id, repository.as_ref());
            permissions.await.unwrap().merge
        }
    };
    assert_eq!(
        merge(master.id.clone(), Some(repo.clone())).await.value,
        Opening::Propose
    );
    assert_eq!(
        merge(master.id.clone(), Some(other.clone())).await.value,
        Opening::Direct
    );
    // Without a repository, the global config applies.
    assert_eq!(merge(master.id.clone(), None).await.value, Opening::Direct);
    // The chat below resolves the master's mode in its own repository.
    let below = merge(child.id.clone(), Some(repo.clone())).await;
    assert_eq!(below.value, Opening::Propose);
    assert_eq!(below.from, PermissionSource::Parent);
}

#[tokio::test]
async fn a_launch_takes_the_mode_it_starts_in_before_the_mode_is_recorded() {
    let fixture = Arc::new(Fixture::new().await);
    let repo = fixture.repository("repo").await;
    let config = "[modes.review.permissions]\npush = 'propose'\n\
                  [modes.chat.permissions]\npush = 'deny'\n";
    fixture.set_local_config(&repo, config);
    let top = chat(&fixture, "top", "chat", Some(&repo), None).await;
    let child = workspace(&fixture, &repo, "child", &top.id, None).await;
    let launch = |mode: &'static str| {
        let (fixture, child) = (fixture.clone(), child.clone());
        async move {
            let mode = mode.into();
            let permissions = fixture.coordination.launch_permissions(
                &**fixture,
                child,
                &mode,
                Default::default(),
            );
            permissions.await.unwrap().push
        }
    };
    let review = launch("review").await;
    assert_eq!(review.value, Pushes::Deny);
    assert_eq!(review.from, PermissionSource::Parent);
    let config = "[modes.review.permissions]\npush = 'propose'\n";
    fixture.set_local_config(&repo, config);
    assert_eq!(launch("review").await.value, Pushes::Propose);
    assert_eq!(launch("worker").await.value, Pushes::Direct);
    // Nothing is recorded: the workspace has no mode of its own yet, so
    // its push is the key's default rather than the review mode's.
    let recorded = fixture
        .coordination
        .space_permissions(&**fixture, &child.id, Some(&repo))
        .await
        .unwrap();
    assert_eq!(recorded.push.value, Pushes::Direct);
}

#[tokio::test]
async fn a_given_permission_applies_to_its_space_and_caps_the_spaces_below() {
    let fixture = Arc::new(Fixture::new().await);
    let repo = fixture.repository("repo").await;
    let top = chat(&fixture, "top", "chat", Some(&repo), None).await;
    let worker = workspace(&fixture, &repo, "worker", &top.id, Some("worker".into())).await;
    let split = workspace(&fixture, &repo, "split", &worker.id, None).await;
    fixture.give(&top.id, &["merge=propose"]);
    // Wider than the worker mode's merge and the chat's above it.
    let assignments = ["push=propose", "merge=direct"];
    fixture.give(&worker.id, &assignments);
    let of = |id: SpaceId| {
        let fixture = fixture.clone();
        let repository = repo.clone();
        async move {
            let permissions =
                fixture
                    .coordination
                    .space_permissions(&**fixture, &id, Some(&repository));
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
    let fixture = Arc::new(Fixture::new().await);
    let repo = fixture.repository("repo").await;
    let worker = fixture
        .workspace_in(repo.as_str(), "worker", None, None)
        .await;
    let child = workspace(&fixture, &repo, "child", &worker.id, Some("worker".into())).await;
    let talk = chat(&fixture, "talk", "chat", Some(&repo), Some(&worker.id)).await;
    // The user gave both a merge the worker does not hold.
    fixture.give(&child.id, &["merge=direct"]);
    fixture.give(&talk.id, &["merge=direct"]);
    let talk = fixture.space_of(&talk.id);
    let assigned = |assignments: &[&str]| {
        let assignments: Vec<String> = assignments.iter().map(|a| (*a).to_owned()).collect();
        crate::config::permissions::PermissionsConfig::assigned(&assignments).unwrap()
    };
    let worker = Caller::of_space(worker.id.clone());
    let (giver, child) = (Some(&worker), SpaceSelector::id(&child.id));

    // What the space recorded comes back, with what the giver holds.
    let recorded = assigned(&["merge=direct", "push=propose"]);
    let registered = fixture
        .coordination
        .ensure_agent_held(&**fixture, giver, &child, &WORKER, &recorded);
    registered.await.unwrap();
    fixture
        .coordination
        .ensure_agent_held(
            &**fixture,
            giver,
            &SpaceSelector::id(&talk.id),
            &CHAT,
            &recorded,
        )
        .await
        .unwrap();
    // A value the giver does not hold, recorded nowhere, is refused.
    let wider = assigned(&["merge=direct", "api_writes=direct"]);
    let registered = fixture
        .coordination
        .ensure_agent_held(&**fixture, giver, &child, &WORKER, &wider);
    assert!(registered.await.is_err());
    assert!(
        fixture
            .coordination
            .ensure_agent_held(
                &**fixture,
                giver,
                &SpaceSelector::id(&talk.id),
                &CHAT,
                &wider
            )
            .await
            .is_err()
    );
    // A restart takes nothing as recorded.
    let change = crate::config::permissions::PermissionChange {
        set: recorded,
        dropped: Vec::new(),
    };
    let restarted =
        fixture
            .coordination
            .ensure_change_held(&**fixture, giver, &talk, &change, Some(&repo));
    assert!(restarted.await.is_err());
    // The user gives any.
    let registered = fixture
        .coordination
        .ensure_agent_held(&**fixture, None, &child, &WORKER, &wider);
    registered.await.unwrap();
    fixture
        .coordination
        .ensure_agent_held(
            &**fixture,
            None,
            &SpaceSelector::id(&talk.id),
            &CHAT,
            &wider,
        )
        .await
        .unwrap();
    let restarted =
        fixture
            .coordination
            .ensure_change_held(&**fixture, None, &talk, &change, Some(&repo));
    restarted.await.unwrap();
}

#[tokio::test]
async fn a_scoped_process_drops_a_key_only_to_a_value_it_holds() {
    let fixture = Arc::new(Fixture::new().await);
    let repo = fixture.repository("repo").await;
    let top = chat(&fixture, "top", "chat", Some(&repo), None).await;
    let parent = workspace(&fixture, &repo, "parent", &top.id, Some("worker".into())).await;
    let child = workspace(&fixture, &repo, "child", &parent.id, Some("worker".into())).await;
    // The user gave the chat less push than the spaces below it.
    fixture.give(&top.id, &["push=propose"]);
    fixture.give(&parent.id, &["push=direct"]);
    fixture.give(&child.id, &["push=propose"]);
    let child = fixture.space_of(&child.id);
    let space = child;
    let dropped = |assignment: &str| {
        crate::config::permissions::PermissionChange::assigned(assignment).unwrap()
    };
    let check = |giver: Option<SpaceId>, change| {
        let (fixture, space, repository) = (fixture.clone(), space.clone(), repo.clone());
        async move {
            let giver = giver.map(Caller::of_space);
            let held = fixture.coordination.ensure_change_held(
                &**fixture,
                giver.as_ref(),
                &space,
                &change,
                Some(&repository),
            );
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
    let talk = chat(&fixture, "talk", "chat", Some(&repo), Some(&parent.id)).await;
    fixture.give(&talk.id, &["push=propose"]);
    let talk = fixture.space_of(&talk.id);
    let none = crate::config::permissions::PermissionsConfig::default();
    let top = Caller::of_space(top.id.clone());
    let (giver, child) = (Some(&top), SpaceSelector::id(&space.id));
    let registered = fixture
        .coordination
        .ensure_agent_held(&**fixture, giver, &child, &WORKER, &none);
    assert!(registered.await.is_err());
    assert!(
        fixture
            .coordination
            .ensure_agent_held(
                &**fixture,
                giver,
                &SpaceSelector::id(&talk.id),
                &CHAT,
                &none
            )
            .await
            .is_err()
    );
    let recorded = talk.narrowing;
    let registered = fixture
        .coordination
        .ensure_agent_held(&**fixture, giver, &child, &WORKER, &recorded);
    registered.await.unwrap();
    fixture
        .coordination
        .ensure_agent_held(
            &**fixture,
            giver,
            &SpaceSelector::id(&talk.id),
            &CHAT,
            &recorded,
        )
        .await
        .unwrap();

    // A dropped key falls back in the mode of the agent that drops it.
    let config = "[modes.review.permissions]\npush = 'deny'\n";
    fixture.set_local_config(&repo, config);
    let quiet = workspace(&fixture, &repo, "quiet", &parent.id, Some("review".into())).await;
    fixture.give(&quiet.id, &["push=deny"]);
    let quiet = SpaceSelector::id(&quiet.id);
    let review = "review".into();
    let registered = fixture
        .coordination
        .ensure_agent_held(&**fixture, giver, &quiet, &review, &none);
    registered.await.unwrap();
    let registered = fixture
        .coordination
        .ensure_agent_held(&**fixture, giver, &quiet, &WORKER, &none);
    assert!(registered.await.is_err());
}

/// The mode the workspaces here record.
static WORKER: crate::config::modes::ModeName = crate::config::modes::ModeName::DEFAULT;
static CHAT: crate::config::modes::ModeName = crate::config::modes::ModeName::CHAT;
