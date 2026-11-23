//! A chat without a repository acting in each registered repository.
use crate::{
    config::permissions::Opening,
    daemon::{
        chats::NewChat, links::proposals::store_issue_proposal, manager::Manager,
        test_support::manager,
    },
    forge::items::create::NewIssue,
    model::{Chat, Repository},
    protocol::{ItemContext, ItemMethod, RepoTarget},
    test_support::repository,
};
use shoal_protocol::space_names::SpaceSelector;

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

/// Start the chat `name` of `repository`, or without one, with its agent
/// running.
async fn chat(
    manager: &std::sync::Arc<Manager>,
    name: &str,
    repository: Option<&Repository>,
) -> Chat {
    let new = NewChat {
        priority: None,
        narrowing: Default::default(),
        name: name.into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        origin: None,
        parent: None,
    };
    let repository = repository.map(|repository| repository.id.clone().into());
    let chat = manager.start_new_chat(repository, None, new).await.unwrap();
    manager.run_chat_agent(chat, None).await.unwrap()
}

/// The context of `chat` acting in `repository`.
fn acting_in(chat: &Chat, repository: &Repository) -> ItemContext {
    ItemContext::Space {
        space: SpaceSelector::id(&chat.id),
        repository: Some(repository.id.clone()),
    }
}

#[tokio::test]
async fn a_chat_without_a_repository_acts_in_each_repository_under_its_permissions() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    let config = "[permissions]\nissue_open = 'propose'\n";
    (manager.write_local_config(&repos[0].id, Some(config.into())))
        .await
        .unwrap();
    let master = chat(&manager, "master", None).await;
    let mut openings = Vec::new();
    for repo in &repos {
        let space = manager
            .links
            .context_space(&*manager, &acting_in(&master, repo))
            .await
            .unwrap();
        assert_eq!(*space.repository_id(), repo.id);
        assert!(!space.keeps_links());
        let permissions = manager.coordination.space_permissions(
            &*manager,
            &master.id,
            Some(space.repository_id()),
        );
        openings.push(permissions.await.unwrap().issue_open.value);
    }
    assert_eq!(openings, [Opening::Propose, Opening::Direct]);

    // Its proposal names the repository it acts in, where the decision
    // finds it again.
    let space = manager
        .links
        .context_space(&*manager, &acting_in(&master, &repos[0]))
        .await
        .unwrap();
    let proposal = store_issue_proposal(&manager, &space).await;
    assert_eq!(proposal.repository_id.as_ref(), Some(&repos[0].id));
    let found = manager
        .links
        .find_link_space(&*manager, &master.id, proposal.repository_id.as_ref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*found.repository_id(), repos[0].id);

    // A chat of a repository acts only in its own.
    let planning = chat(&manager, "planning", Some(&repos[0])).await;
    let elsewhere = manager
        .links
        .context_space(&*manager, &acting_in(&planning, &repos[1]))
        .await;
    assert!(elsewhere.is_err());
}

#[tokio::test]
async fn a_space_naming_its_own_repository_acts_there_as_itself() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    let workspace = manager
        .create_workspace(&repos[0].id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let context = |repository: &Repository| ItemContext::Space {
        space: SpaceSelector::id(&workspace.id),
        repository: Some(repository.id.clone()),
    };
    let own = manager
        .links
        .context_space(&*manager, &context(&repos[0]))
        .await
        .unwrap();
    assert_eq!(
        (
            own.id.clone(),
            own.repository_id().clone(),
            own.keeps_links()
        ),
        (workspace.id.clone(), repos[0].id.clone(), true)
    );
    assert!(
        manager
            .links
            .context_space(&*manager, &context(&repos[1]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_name_or_url_finds_the_registered_repository_it_names() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    for (repo, origin) in repos.iter().zip(["team/repo", "team/other"]) {
        let url = format!("https://github.com/{origin}");
        crate::test_support::git(&repo.path, &["remote", "add", "origin", &url]);
    }
    for (named, found) in [
        ("team/other", Some(&repos[1].id)),
        ("https://github.com/team/repo", Some(&repos[0].id)),
        ("team/unregistered", None),
    ] {
        let registered = manager.registered_named(Some(named)).await.unwrap();
        assert_eq!(registered.as_ref().map(|repo| &repo.id), found, "{named}");
    }
    // A name two registrations answer to names neither.
    let third = repositories(&temp, &manager, &["copy"]).await.remove(0);
    let url = "https://github.com/team/repo";
    crate::test_support::git(&third.path, &["remote", "add", "origin", url]);
    assert!(manager.registered_named(Some("team/repo")).await.is_err());
    // A checkout whose origin names no forge fails the lookup rather than
    // passing for a repository that is not registered.
    let broken = repositories(&temp, &manager, &["broken"]).await.remove(0);
    let origin = "not a forge remote";
    crate::test_support::git(&broken.path, &["remote", "add", "origin", origin]);
    let found = manager.registered_named(Some("team/unregistered")).await;
    assert!(found.is_err(), "{found:?}");
}

#[tokio::test]
async fn a_change_from_a_checkout_is_made_by_a_chat_without_a_repository_acting_there() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo"]).await;
    let master = chat(&manager, "master", None).await;
    let planning = chat(&manager, "planning", Some(&repos[0])).await;
    let open = || ItemMethod::OpenIssue {
        target: ItemContext::Checkout(repos[0].id.clone()).into(),
        issue: NewIssue {
            title: "Flaky".into(),
            ..NewIssue::default()
        },
        link: false,
    };
    let token = manager.chat_scope(&master.id).await.unwrap();
    let mut method = open();
    crate::daemon::scope::authorize(&manager, Some(&token), &mut method)
        .await
        .unwrap();
    let ItemMethod::OpenIssue { target, .. } = method else {
        unreachable!("the method stays an issue opening");
    };
    let Some(ItemContext::Space {
        space: chat,
        repository: Some(repository),
    }) = target.context
    else {
        panic!("expected the chat acting in the checkout: {target:?}");
    };
    assert_eq!(
        (chat, repository),
        (SpaceSelector::id(&master.id), repos[0].id.clone())
    );
    let scoped = manager.chat_scope(&planning.id).await.unwrap();
    let refused = crate::daemon::scope::authorize(&manager, Some(&scoped), &mut open()).await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_chat_without_a_repository_follows_ci_in_a_registered_checkout_as_itself() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    let master = chat(&manager, "master", None).await;
    let planning = chat(&manager, "planning", Some(&repos[0])).await;
    let list = || crate::protocol::CiMethod::ListCiRuns {
        target: ItemContext::Checkout(repos[1].id.clone()).into(),
        pr: None,
        filter: crate::forge::ci::run_view::RunFilter {
            branch: None,
            workflow: None,
            commit: None,
            limit: 10,
        },
    };
    let token = manager.chat_scope(&master.id).await.unwrap();
    let mut method = list();
    crate::daemon::scope::authorize(&manager, Some(&token), &mut method)
        .await
        .unwrap();
    let crate::protocol::CiMethod::ListCiRuns { target, .. } = method else {
        unreachable!("the method stays a run listing");
    };
    let Some(ItemContext::Space {
        space: chat,
        repository: Some(repository),
    }) = target.context
    else {
        panic!("expected the chat acting in the checkout: {target:?}");
    };
    assert_eq!(
        (chat, repository),
        (SpaceSelector::id(&master.id), repos[1].id.clone())
    );
    let scoped = manager.chat_scope(&planning.id).await.unwrap();
    let refused = crate::daemon::scope::authorize(&manager, Some(&scoped), &mut list()).await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_chat_without_a_repository_opens_no_issue_elsewhere_unless_allowed_directly() {
    let (_temp, manager) = manager().await;
    let master = chat(&manager, "master", None).await;
    let caller = crate::daemon::scope::Caller {
        space_id: master.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let issue = || NewIssue {
        title: "Flaky".into(),
        ..NewIssue::default()
    };
    for setting in ["propose", "deny"] {
        let config = format!("[permissions]\nissue_other_repositories = '{setting}'\n");
        crate::daemon::test_support::configure(&manager, &config).await;
        let opened = manager
            .links
            .open_issue(
                &*manager,
                &RepoTarget {
                    context: Some(ItemContext::space(SpaceSelector::id(&master.id))),
                    repository: Some("https://github.com/other/unregistered".into()),
                },
                issue(),
                false,
                Some(&caller),
            )
            .await;
        // Refused for the setting, before any request to the forge.
        let error = format!("{:#}", opened.err().unwrap());
        assert!(
            error.contains("issue_other_repositories"),
            "{setting}: {error}"
        );
    }
}
