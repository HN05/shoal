use crate::daemon::manager::Manager;
use crate::daemon::scope::Caller;
use shoal_protocol::launches::Launch;
use shoal_protocol::launches::LaunchSource;
use shoal_protocol::launches::SpaceChoice;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::test_support::{global_config, manager, repository},
    model::Repository,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::launches::StartTarget;

/// A repository and an agent that starts without limits.
async fn machine(manager: &Manager, temp: &tempfile::TempDir) -> Repository {
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false\n\
         [agents.claude]\ncommand = ['sh']\n",
    ));
    let path = repository(temp.path(), "repo");
    let path = path.to_str().unwrap().into();
    manager
        .repositories
        .register(path, None, None)
        .await
        .unwrap()
}

/// The launch of a chat about `repo` by the agent `claude`.
fn chat_launch(repo: &Repository) -> Launch {
    Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        mode: Some("chat".into()),
        agent: Some("claude".into()),
        ..Launch::default()
    }
}

/// Whether the user interacted with the agent of the space `id`.
async fn engaged(manager: &Manager, id: &SpaceId) -> bool {
    manager
        .agents
        .interactions()
        .await
        .unwrap()
        .contains_key(id)
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

    (manager
        .spaces
        .launch(&manager.daemon(), launch, LaunchSource::Manual, None))
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

    let launched = (manager.spaces.launch(
        &manager.daemon(),
        chat_launch(&repo),
        LaunchSource::Manual,
        None,
    ))
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

    let triggered = manager
        .spaces
        .launch(&manager.daemon(), chat_launch(&repo), trigger, None)
        .await;
    let scoped = (manager.spaces.launch(
        &manager.daemon(),
        chat_launch(&repo),
        LaunchSource::Manual,
        Some(&caller),
    ))
    .await;

    for launched in [triggered.unwrap(), scoped.unwrap()] {
        assert!(!engaged(&manager, &launched.space.id).await);
    }
}

/// A scoped caller on another machine, which this one has no space of,
/// reads a launch's items through the repository's checkout.
#[tokio::test]
async fn another_machine_s_caller_reads_a_launch_s_items_through_the_checkout() {
    use shoal_protocol::hub::routing::VouchedSpace;

    let daemon = crate::daemon::test_daemon::TestDaemon::start().await;
    let manager = &daemon.manager;
    let workspace = daemon.workspace("here").await;
    let repository = &daemon.repository.id;
    let permissions =
        manager
            .coordination
            .space_permissions(&**manager, &workspace.id, Some(repository));
    let space = SpaceId::generate();
    let vouched = VouchedSpace {
        space: space.clone(),
        everywhere: false,
        permissions: permissions.await.unwrap(),
        granted: None,
        read_only: false,
        priority: shoal_schedule::priority::Priority::NORMAL,
        chat_modes: Vec::new(),
    };
    let caller = Caller {
        space_id: space,
        execution: None,
        granted: None,
        read_only: false,
        vouched: Some(std::sync::Arc::new(vouched)),
    };
    let context = manager
        .spaces
        .launch_read_context(repository, Some(&caller));
    let context = context.await.unwrap();
    assert!(
        matches!(&context, crate::protocol::ItemContext::Checkout(id) if id == repository),
        "{context:?}"
    );
}
