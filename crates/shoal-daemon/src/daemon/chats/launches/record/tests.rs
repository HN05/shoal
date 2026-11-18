use crate::{
    daemon::{
        chats::NewChat,
        notifications::{Notification, NotificationKind},
        test_support::{git, global_config, manager, repository},
        workspace::Manager,
    },
    model::{LinkedItem, Repository, SpaceLinks},
    protocol::ChatStart,
};
use shoal_protocol::{chats::LaunchedChat, space_names::SpaceSelector};

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

/// Record the chat `start` describes, below `parent`, as a caller that
/// runs its agent itself does.
async fn record(
    manager: &Manager,
    start: ChatStart,
    parent: Option<&SpaceSelector>,
) -> LaunchedChat {
    let parent = manager.space_id(parent).await.unwrap();
    let new = NewChat::of(&start, parent, None).unwrap();
    manager
        .record_launch(&start, new, None, None)
        .await
        .unwrap()
}

/// An item the start cannot link warns the starter, here the user, and the
/// answer carries the warning, while the chat starts without it.
#[tokio::test]
async fn a_link_that_fails_during_a_start_reaches_the_user() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp, false).await;
    let elsewhere = "https://github.com/other/place/issues/2";

    let started = record(&manager, start(Some(&repo), "linking", &[elsewhere]), None).await;

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

/// What starting a chat below a space whose agent runs warns of reaches
/// that agent instead of the user.
#[tokio::test]
async fn a_link_that_fails_during_a_start_reaches_the_starting_agent() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp, false).await;
    let starter = manager.start_chat((&repo.id).into(), "starter".into(), "claude".into(), None);
    let starter = starter.await.unwrap();
    let elsewhere = "https://github.com/other/place/issues/2";
    let parent = SpaceSelector::id(&starter.id);
    record(
        &manager,
        start(Some(&repo), "linking", &[elsewhere]),
        Some(&parent),
    )
    .await;

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
