use crate::{
    daemon::{
        chats::launches::fake,
        notifications::{Notification, NotificationKind},
        test_support::{git, global_config, manager, repository},
        workspace::Manager,
    },
    model::{LinkedItem, Repository, SpaceLinks},
    protocol::ChatStart,
};
use shoal_protocol::{
    launches::{LaunchSource, Started},
    space_names::SpaceSelector,
};

const ISSUE: &str = "https://github.com/team/repo/issues/316";

/// A registered repository whose remote is a forge's, which Shoal never
/// reaches, and agents and modes that start chats with and without one,
/// without heavy slots unless `heavy`.
async fn machine(manager: &Manager, temp: &tempfile::TempDir, heavy: bool) -> Repository {
    let limit = match heavy {
        true => "1",
        false => "'none'",
    };
    manager.publish_config(global_config(&format!(
        "[machine]\nmax_heavy_agents = {limit}\n[machine.overload.disk]\nenabled = false\n\
         [modes.solo]\nkind = 'chat'\nrepository = false\n\
         [agents.claude]\ncommand = ['sh']\n"
    )));
    let path = repository(temp.path(), "repo");
    git(&path, &["config", "protocol.allow", "never"]);
    git(
        &path,
        &["remote", "add", "origin", "https://github.com/team/repo"],
    );
    let path = path.to_str().unwrap().into();
    manager.register_repository(path, None, None).await.unwrap()
}

/// The start of a chat named `name` in `repository`, or in none, linked to
/// `issues`.
fn start(repository: Option<&Repository>, name: &str, issues: &[&str]) -> ChatStart {
    let issues = (issues.iter())
        .map(|url| LinkedItem {
            url: url.parse().unwrap(),
            title: None,
        })
        .collect();
    ChatStart {
        repository: repository.map(|repository| (&repository.id).into()),
        agent: Some("claude".into()),
        mode: Some(match repository {
            Some(_) => "chat".into(),
            None => "solo".into(),
        }),
        name: Some(name.into()),
        links: SpaceLinks {
            issues,
            ..SpaceLinks::default()
        },
        ..ChatStart::default()
    }
}

/// The user's notifications of `kind`.
async fn notified(manager: &Manager, kind: NotificationKind) -> Vec<Notification> {
    let notifications = manager.notifications(true, 50).await.unwrap();
    (notifications.into_iter())
        .filter(|notification| notification.kind == kind)
        .collect()
}

/// A client's chat on an issue, which it names by number, is recorded with
/// the issue linked before its place starts.
#[tokio::test]
async fn a_launched_chat_is_recorded_with_its_issue_before_its_place_starts() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp, false).await;

    let on_issue = ChatStart {
        issue: Some("316".parse().unwrap()),
        ..start(Some(&repo), "planning", &[])
    };

    let started = manager.launch_chat(on_issue, LaunchSource::Manual, None);
    let started = started.await.unwrap();

    let Started::Background { place } = &started.start else {
        panic!("expected a chat in a background place: {started:?}");
    };
    assert_eq!(place, "background");
    assert!(started.warnings.is_empty(), "{:?}", started.warnings);
    let chats = manager.chats().await.unwrap();
    let chat = chats.iter().find(|chat| chat.name == "planning").unwrap();
    let linked: Vec<_> = chat
        .links
        .issues
        .iter()
        .map(|issue| issue.url.url())
        .collect();
    assert_eq!(linked, [ISSUE]);
    // The answer names the chat as the daemon recorded it, linked.
    assert_eq!(started.space.id, chat.id);
    assert_eq!(started.space.links, chat.links);
    assert_eq!(started.entry, None);
}

/// A chat whose agent an app runs detached answers with the chat it
/// recorded and the app's run.
#[tokio::test]
async fn a_launched_chat_an_app_runs_answers_with_its_chat_and_run() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp, false).await;
    fake::detach(&manager.paths, "app talk".into());

    let started = manager.launch_chat(
        start(Some(&repo), "app talk", &[]),
        LaunchSource::Manual,
        None,
    );
    let started = started.await.unwrap();

    let Started::Detached(run) = &started.start else {
        panic!("expected a chat an app runs: {started:?}");
    };
    assert_eq!(run.log, std::path::Path::new("app talk.log"));
    let chats = manager.chats().await.unwrap();
    let chat = chats.iter().find(|chat| chat.name == "app talk").unwrap();
    assert_eq!(started.space.id, chat.id);
    assert_eq!(started.space.kind(), crate::model::SpaceKind::Chat);
}

/// A chat without a repository that starts on nothing links nothing and
/// warns of nothing.
#[tokio::test]
async fn a_chat_without_a_repository_links_nothing_and_warns_of_nothing() {
    let (temp, manager) = manager().await;
    machine(&manager, &temp, false).await;

    let started = manager.launch_chat(
        start(None, "orchestrating", &[]),
        LaunchSource::Manual,
        None,
    );
    let started = started.await.unwrap();

    assert!(started.warnings.is_empty(), "{:?}", started.warnings);
    assert!(
        notified(&manager, NotificationKind::StartWarned)
            .await
            .is_empty()
    );
    let chats = manager.chats().await.unwrap();
    let chat = chats
        .iter()
        .find(|chat| chat.name == "orchestrating")
        .unwrap();
    assert!(chat.links.is_empty());
}

/// An item the start cannot link warns the starter, here the user, and the
/// answer carries the warning, while the chat starts without it.
#[tokio::test]
async fn a_link_that_fails_during_a_start_reaches_the_user() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp, false).await;
    let elsewhere = "https://github.com/other/place/issues/2";

    let started = manager.launch_chat(
        start(Some(&repo), "linking", &[elsewhere]),
        LaunchSource::Manual,
        None,
    );
    let started = started.await.unwrap();

    assert!(
        started
            .warnings
            .iter()
            .any(|warning| warning.contains(elsewhere)),
        "{:?}",
        started.warnings
    );
    let warned = notified(&manager, NotificationKind::StartWarned).await;
    assert!(
        warned
            .iter()
            .any(|notification| notification.message.contains(elsewhere)),
        "{warned:?}"
    );
}

/// Start a chat named `name` in `repo` whose place fails, and check that
/// the daemon removed it and told the user why.
async fn check_failed_place_removes_the_chat(manager: &Manager, repo: &Repository, name: &str) {
    fake::fail(&manager.paths, name.into());

    let started = manager.launch_chat(
        start(Some(repo), name, &[ISSUE]),
        LaunchSource::Manual,
        None,
    );
    let error = started.await.unwrap_err();

    assert!(
        format!("{error:#}").contains("ended before the chat started"),
        "{error:#}"
    );
    let chats = manager.chats().await.unwrap();
    assert!(chats.iter().all(|chat| chat.name != name));
    let failed = notified(manager, NotificationKind::StartFailed).await;
    assert!(
        failed
            .iter()
            .any(|notification| notification.message.contains(name)),
        "{failed:?}"
    );
}

/// A chat whose place fails is removed, its failure told, where no heavy
/// slot tracks its agent.
#[tokio::test]
async fn an_untracked_chat_whose_place_fails_is_removed() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp, false).await;
    check_failed_place_removes_the_chat(&manager, &repo, "untracked").await;
}

/// A chat whose place fails is removed with the entry that held its
/// agent's slot, which frees the slot.
#[tokio::test]
async fn a_chat_whose_place_fails_is_removed_with_its_slot() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp, true).await;
    check_failed_place_removes_the_chat(&manager, &repo, "tracked").await;
    assert!(manager.queue_entries().await.unwrap().is_empty());
}

/// What starting a chat below a space whose agent runs warns of reaches
/// that agent instead of the user.
#[tokio::test]
async fn a_link_that_fails_during_a_start_reaches_the_starting_agent() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp, false).await;
    let starter = manager.start_chat((&repo.id).into(), "starter".into(), "claude".into(), None);
    let starter = starter.await.unwrap();
    let elsewhere = "https://github.com/other/place/issues/2";
    let child = ChatStart {
        parent: Some(SpaceSelector::id(&starter.id)),
        ..start(Some(&repo), "linking", &[elsewhere])
    };

    (manager.launch_chat(child, LaunchSource::Manual, None))
        .await
        .unwrap();

    let starter = SpaceSelector::id(&starter.id);
    let messages = manager.agent_messages(&starter).await.unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.message.contains(elsewhere)),
        "{messages:?}"
    );
    assert!(
        notified(&manager, NotificationKind::StartWarned)
            .await
            .is_empty()
    );
}

/// A chat recorded for a start that failed before its agent ran is removed,
/// its failure told, and one removed already leaves nothing to do, while
/// one whose agent started stays, even once its execution's record went
/// with its completion.
#[tokio::test]
async fn a_failed_start_removes_only_a_new_chat_whose_agent_never_started() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp, false).await;
    let new = crate::daemon::chats::NewChat {
        name: "unopened".into(),
        agent: "claude".into(),
        mode: "chat".into(),
        narrowing: Default::default(),
        origin: None,
        parent: None,
        priority: None,
    };
    let repository = Some((&repo.id).into());
    let unopened = manager.start_new_chat(repository, None, new).await.unwrap();
    let ran = crate::daemon::test_support::chat(&manager, "ran", "chat", Some(&repo), None).await;
    // Its wrapper's registration journals the start, which outlasts the
    // execution's record.
    let started = shoal_protocol::events::AgentEvent {
        name: "claude".into(),
        execution_id: None,
        exit_code: None,
        source: None,
    };
    let kind = crate::daemon::events::EventKind::AgentStarted;
    manager.record_agent(&ran.id, kind, started).await;
    let ran_id = ran.id.clone();
    (manager.store)
        .write(move |tx| Ok(tx.execute("DELETE FROM executions WHERE space_id=?1", [&ran_id])?))
        .await
        .unwrap();

    manager
        .fail_unstarted_chat(&unopened.id, "no tab")
        .await
        .unwrap();
    manager
        .fail_unstarted_chat(&unopened.id, "no tab")
        .await
        .unwrap();
    manager
        .fail_unstarted_chat(&ran.id, "no tab")
        .await
        .unwrap();

    let chats = manager.chats().await.unwrap();
    let names: Vec<_> = chats.iter().map(|chat| chat.name.as_str()).collect();
    assert_eq!(names, ["ran"]);
    let told = notified(&manager, NotificationKind::StartFailed).await;
    assert!(
        told.iter().any(|told| told.message.contains("unopened")),
        "{told:?}"
    );
}
