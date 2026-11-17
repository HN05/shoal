use crate::{
    daemon::{
        notifications::{Notification, NotificationKind},
        test_support::{git, global_config, manager, repository},
        workspace::Manager,
    },
    model::{LinkedItem, Repository, SpaceLinks},
    protocol::ChatStart,
};
use shoal_protocol::{chats::StartedChat, launches::LaunchSource, space_names::SpaceSelector};

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

    let StartedChat::Background(background) = started else {
        panic!("expected a chat in a background place: {started:?}");
    };
    assert!(background.warnings.is_empty(), "{:?}", background.warnings);
    let chats = manager.chats().await.unwrap();
    let chat = chats.iter().find(|chat| chat.name == "planning").unwrap();
    let linked: Vec<_> = chat
        .links
        .issues
        .iter()
        .map(|issue| issue.url.url())
        .collect();
    assert_eq!(linked, [ISSUE]);
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
    let StartedChat::Background(background) = started.await.unwrap() else {
        panic!("expected a chat in a background place");
    };

    assert!(background.warnings.is_empty(), "{:?}", background.warnings);
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
    let StartedChat::Background(background) = started.await.unwrap() else {
        panic!("expected a chat in a background place");
    };

    assert!(
        background
            .warnings
            .iter()
            .any(|warning| warning.contains(elsewhere)),
        "{:?}",
        background.warnings
    );
    let warned = notified(&manager, NotificationKind::StartWarned).await;
    assert!(
        warned
            .iter()
            .any(|notification| notification.message.contains(elsewhere)),
        "{warned:?}"
    );
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
