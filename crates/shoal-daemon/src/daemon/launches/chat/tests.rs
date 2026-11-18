use super::*;
use crate::daemon::test_support::{global_config, manager, repository};
use shoal_protocol::launches::{LaunchSource, StartTarget};

/// A launch that starts a chat hands its prompt to the chat's start, which
/// its chat worker gives the agent as its first message.
#[tokio::test]
async fn a_chat_launch_carries_its_prompt_to_the_chat_start() {
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

    (manager.launch(launch, LaunchSource::Manual, None))
        .await
        .unwrap();

    let started = crate::daemon::chats::launches::fake::started(&manager.paths);
    let start = (started.iter())
        .map(|(start, _)| start)
        .find(|start| start.name == Some("prompted talk".into()))
        .expect("the daemon ran the chat's start worker");
    assert_eq!(start.prompt.as_deref(), Some("Set up the repository"));
}
