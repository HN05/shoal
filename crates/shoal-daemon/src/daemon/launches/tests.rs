use super::*;
use crate::{
    daemon::test_support::{global_config, manager, repository},
    model::Repository,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::launches::StartTarget;

/// A repository and an agent that starts without limits.
async fn machine(manager: &Manager, temp: &tempfile::TempDir) -> Repository {
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 'none'\n[machine.overload.disk]\nenabled = false\n\
         [agents.claude]\ncommand = ['sh']\n",
    ));
    let path = repository(temp.path(), "repo");
    let path = path.to_str().unwrap().into();
    manager.register_repository(path, None, None).await.unwrap()
}

/// The launch of a chat about `repo` by the agent `claude`.
fn chat_launch(repo: &Repository) -> Launch {
    Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        chat: true,
        agent: Some("claude".into()),
        ..Launch::default()
    }
}

/// Whether the user interacted with the agent of the space `id`.
async fn engaged(manager: &Manager, id: &SpaceId) -> bool {
    manager.interactions().await.unwrap().contains_key(id)
}

/// A detached start of an existing workspace's agent that the user makes
/// is their interaction with that agent.
#[tokio::test]
async fn the_users_start_of_a_workspaces_agent_engages_them() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp).await;
    let workspace = manager.create_workspace(&repo.id, "work".into(), None, None, None);
    let workspace = workspace.await.unwrap();
    let launch = Launch {
        in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&workspace.id))),
        agent: Some("claude".into()),
        task: vec!["Fix the login".into()],
        ..Launch::default()
    };

    (manager.launch(launch, LaunchSource::Manual, None))
        .await
        .unwrap();

    assert!(engaged(&manager, &workspace.id).await);
}

/// A new chat the user starts from their terminal engages them with its
/// agent.
#[tokio::test]
async fn the_users_new_chat_engages_them() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp).await;

    let launched = (manager.launch(chat_launch(&repo), LaunchSource::Manual, None))
        .await
        .unwrap();

    assert!(engaged(&manager, &launched.space.id).await);
}

/// Neither a trigger's start nor a scoped caller's is the user's
/// interaction with the agent it starts.
#[tokio::test]
async fn a_trigger_or_scoped_start_leaves_the_user_unengaged() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp).await;
    let parent = crate::daemon::test_support::chat(&manager, "parent", "chat", Some(&repo), None);
    let parent = parent.await;
    let caller = Caller {
        space_id: parent.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let trigger = LaunchSource::Trigger {
        name: "nightly".into(),
    };

    let triggered = manager.launch(chat_launch(&repo), trigger, None).await;
    let scoped = (manager.launch(chat_launch(&repo), LaunchSource::Manual, Some(&caller))).await;

    for launched in [triggered.unwrap(), scoped.unwrap()] {
        assert!(!engaged(&manager, &launched.space.id).await);
    }
}
