//! A chat without a repository acting in each registered repository.
use crate::{
    config::repo::Opening,
    daemon::test_support::manager,
    daemon::{chats::NewChat, links::proposals::tests::store_issue_proposal, workspace::Manager},
    forge::items::create::NewIssue,
    model::{Chat, Repository},
    protocol::{ItemContext, ItemMethod},
    test_support::{ProcessGroup, repository},
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

/// Start the chat `name` of `repository`, or without one, running as the
/// returned process.
async fn chat(
    manager: &Manager,
    name: &str,
    repository: Option<&Repository>,
) -> (Chat, ProcessGroup) {
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let new = NewChat {
        name: name.into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        pid: process.id(),
        origin: None,
        hands_over: false,
        parent: None,
    };
    let repository = repository.map(|repository| repository.id.clone().into());
    let chat = manager.start_new_chat(repository, None, new).await.unwrap();
    (chat, process)
}

/// The context of `chat` acting in `repository`.
fn acting_in(chat: &Chat, repository: &Repository) -> ItemContext {
    ItemContext::ChatIn {
        chat: chat.id.clone(),
        repository: repository.id.clone(),
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
    let (master, _process) = chat(&manager, "master", None).await;
    let mut openings = Vec::new();
    for repo in &repos {
        let space = manager
            .context_space(&acting_in(&master, repo))
            .await
            .unwrap();
        assert_eq!(space.repository_id(), repo.id);
        assert!(!space.keeps_links());
        let settings = manager.space_settings(&space).await.unwrap();
        openings.push(settings.permissions.issue_open);
    }
    assert_eq!(openings, [Opening::Propose, Opening::Direct]);

    // A request resolving its space from the context keeps the checkout it
    // acts in, as a check log's does.
    let context = acting_in(&master, &repos[1]);
    let space = manager.context_space_if_any(&context).await.unwrap();
    assert_eq!(
        space.map(|space| space.repository_id().to_owned()),
        Some(repos[1].id.clone())
    );
    let checkout = ItemContext::Checkout(repos[1].id.clone());
    assert!(
        manager
            .context_space_if_any(&checkout)
            .await
            .unwrap()
            .is_none()
    );

    // Its proposal names the repository it acts in, where the decision
    // finds it again.
    let space = manager
        .context_space(&acting_in(&master, &repos[0]))
        .await
        .unwrap();
    let proposal = store_issue_proposal(&manager, &space).await;
    assert_eq!(
        proposal.repository_id.as_deref(),
        Some(repos[0].id.as_str())
    );
    let found = manager
        .find_link_space(&master.id, proposal.repository_id.as_deref())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.repository_id(), repos[0].id);

    // A chat of a repository acts only in its own.
    let (planning, _planning_process) = chat(&manager, "planning", Some(&repos[0])).await;
    let elsewhere = manager
        .context_space(&acting_in(&planning, &repos[1]))
        .await;
    assert!(elsewhere.is_err());
    let checkout = manager
        .context_checkout(&acting_in(&planning, &repos[1]))
        .await;
    assert!(checkout.is_err());
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
    let (master, _process) = chat(&manager, "master", None).await;
    let (planning, _planning_process) = chat(&manager, "planning", Some(&repos[0])).await;
    let open = || ItemMethod::OpenIssue {
        context: ItemContext::Checkout(repos[0].id.clone()),
        issue: NewIssue {
            title: "Flaky".into(),
            ..NewIssue::default()
        },
        repository: None,
        link: false,
    };
    let token = manager.chat_scope(&master.id).await.unwrap();
    let mut method = open();
    crate::daemon::scope::authorize(&manager, Some(&token), &mut method)
        .await
        .unwrap();
    let ItemMethod::OpenIssue { context, .. } = method else {
        unreachable!("the method stays an issue opening");
    };
    let ItemContext::ChatIn { chat, repository } = context else {
        panic!("expected the chat acting in the checkout: {context:?}");
    };
    assert_eq!((chat, repository), (master.id, repos[0].id.clone()));
    let scoped = manager.chat_scope(&planning.id).await.unwrap();
    let refused = crate::daemon::scope::authorize(&manager, Some(&scoped), &mut open()).await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_chat_without_a_repository_follows_ci_in_a_registered_checkout_as_itself() {
    let (temp, manager) = manager().await;
    let repos = repositories(&temp, &manager, &["repo", "other"]).await;
    let (master, _process) = chat(&manager, "master", None).await;
    let (planning, _planning_process) = chat(&manager, "planning", Some(&repos[0])).await;
    let list = || crate::protocol::CiMethod::ListCiRuns {
        context: ItemContext::Checkout(repos[1].id.clone()),
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
    let crate::protocol::CiMethod::ListCiRuns { context, .. } = method else {
        unreachable!("the method stays a run listing");
    };
    let ItemContext::ChatIn { chat, repository } = context else {
        panic!("expected the chat acting in the checkout: {context:?}");
    };
    assert_eq!((chat, repository), (master.id, repos[1].id.clone()));
    let scoped = manager.chat_scope(&planning.id).await.unwrap();
    let refused = crate::daemon::scope::authorize(&manager, Some(&scoped), &mut list()).await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_chat_without_a_repository_opens_no_issue_elsewhere_unless_allowed_directly() {
    let (_temp, manager) = manager().await;
    let (master, _process) = chat(&manager, "master", None).await;
    let caller = crate::daemon::scope::Caller {
        space_id: master.id.clone(),
        execution: None,
    };
    let issue = || NewIssue {
        title: "Flaky".into(),
        ..NewIssue::default()
    };
    for setting in ["propose", "deny"] {
        let config = format!("[permissions]\nissue_other_repositories = '{setting}'\n");
        crate::daemon::test_support::configure(&manager, &config).await;
        let opened = manager
            .open_issue(
                &ItemContext::Chat(master.id.clone()),
                issue(),
                Some("https://github.com/other/unregistered".into()),
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
