use super::*;
use crate::daemon::test_support::{global_config, manager, repository};
use shoal_protocol::launches::StartTarget;

/// A launch that starts a chat records it and queues its agent with the
/// prompt, which the queue's start gives the agent as its first message.
#[tokio::test]
async fn a_chat_launch_queues_its_agent_with_its_prompt() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager.publish_config(global_config("[agents.claude]\ncommand = ['sh']\n"));
    let launch = Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        chat: true,
        name: Some("prompted talk".into()),
        agent: Some("claude".into()),
        prompt: Some("Set up the repository".into()),
        ..Launch::default()
    };

    let launched = (manager.launch(launch, LaunchSource::Manual, None))
        .await
        .unwrap();

    assert_eq!(launched.space.kind(), SpaceKind::Chat);
    let entry = launched.entry.unwrap();
    assert_eq!(entry.space(), Some(&launched.space.id));
    let plan = manager.queued_plan(&entry.id).await.unwrap().plan;
    let start = crate::start_plan::QueuedStart::of(&plan).unwrap();
    assert_eq!(
        start.launch.prompt.as_deref(),
        Some("Set up the repository")
    );
    let started = crate::daemon::queued_starts::fake::STARTED
        .lock()
        .unwrap()
        .clone();
    assert!(started.iter().any(|(id, _)| *id == entry.id));
}

/// A registered repository whose remote is a forge's, which Shoal never
/// reaches, and agents and modes that start chats with and without one.
async fn offline_machine(manager: &Manager, temp: &tempfile::TempDir) -> crate::model::Repository {
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 'none'\n[machine.overload.disk]\nenabled = false\n\
         [modes.solo]\nkind = 'chat'\nrepository = false\n\
         [agents.claude]\ncommand = ['sh']\n",
    ));
    let path = repository(temp.path(), "repo");
    crate::daemon::test_support::git(&path, &["config", "protocol.allow", "never"]);
    let remote = ["remote", "add", "origin", "https://github.com/team/repo"];
    crate::daemon::test_support::git(&path, &remote);
    let path = path.to_str().unwrap().into();
    manager.register_repository(path, None, None).await.unwrap()
}

/// A chat a launch names on an issue it names by number, which it cannot
/// read, is recorded linked to that issue before its agent starts, and the
/// answer names the chat as recorded with its agent's entry.
#[tokio::test]
async fn a_launched_chat_is_recorded_with_its_issue_before_its_agent_starts() {
    let (temp, manager) = manager().await;
    let repo = offline_machine(&manager, &temp).await;
    let launch = Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        chat: true,
        name: Some("planning".into()),
        agent: Some("claude".into()),
        issue: Some(IssueChoice::Named("316".parse().unwrap())),
        ..Launch::default()
    };

    let launched = manager
        .launch(launch, LaunchSource::Manual, None)
        .await
        .unwrap();

    let chats = manager.chats().await.unwrap();
    let chat = chats.iter().find(|chat| chat.name == "planning").unwrap();
    let linked: Vec<_> = (chat.links.issues.iter())
        .map(|issue| issue.url.url())
        .collect();
    assert_eq!(linked, ["https://github.com/team/repo/issues/316"]);
    assert_eq!(launched.space.id, chat.id);
    assert_eq!(launched.space.links, chat.links);
    assert_eq!(launched.entry.unwrap().space(), Some(&chat.id));
}

/// A chat without a repository that starts on nothing links nothing and
/// warns of nothing.
#[tokio::test]
async fn a_chat_without_a_repository_links_nothing_and_warns_of_nothing() {
    let (temp, manager) = manager().await;
    offline_machine(&manager, &temp).await;
    let launch = Launch {
        mode: Some("solo".into()),
        name: Some("orchestrating".into()),
        agent: Some("claude".into()),
        ..Launch::default()
    };

    let launched = manager
        .launch(launch, LaunchSource::Manual, None)
        .await
        .unwrap();

    assert!(launched.warnings.is_empty(), "{:?}", launched.warnings);
    assert_eq!(launched.space.repository_id, None);
    let chats = manager.chats().await.unwrap();
    let chat = (chats.iter())
        .find(|chat| chat.name == "orchestrating")
        .unwrap();
    assert!(chat.links.is_empty());
}
