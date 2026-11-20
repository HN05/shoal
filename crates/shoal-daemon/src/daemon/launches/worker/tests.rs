use super::*;
use crate::{
    daemon::{
        agent_queue::StoppedAgent,
        test_support::{global_config, manager, repository},
    },
    execution::recovery,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::launches::StartTarget;

/// A workspace whose agent stopped and awaits `shoal resume`, on a machine
/// that starts agents without limits, with the path of its agent's record.
async fn stopped_workspace(
    manager: &Manager,
    temp: &tempfile::TempDir,
) -> (Workspace, std::path::PathBuf) {
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false\n\
         [agents.claude]\ncommand = ['sh']\n",
    ));
    let path = repository(temp.path(), "repo");
    let path = path.to_str().unwrap().into();
    let repo = manager.register_repository(path, None, None).await.unwrap();
    let workspace = manager.create_workspace(&repo.id, "work".into(), None, None, None);
    let workspace = workspace.await.unwrap();
    let record = recovery::save_record(
        &manager.paths,
        &workspace.id,
        &ExecutionId::from("stopped-agent".to_owned()),
        &shoal_protocol::executions::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    (workspace, record)
}

/// A start of the agent of `workspace` on a task in the worker mode, as
/// `shoal start --in` sends it once the mode is named, replacing its
/// stopped agent when `replace`.
fn start_in(workspace: &Workspace, replace: bool) -> Launch {
    Launch {
        in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&workspace.id))),
        mode: Some(crate::config::modes::ModeName::DEFAULT),
        agent: Some("claude".into()),
        task: vec!["Fix the login".into()],
        replace,
        ..Launch::default()
    }
}

#[tokio::test]
async fn a_start_without_replace_is_refused_by_a_stopped_agent() {
    let (temp, manager) = manager().await;
    let (workspace, record) = stopped_workspace(&manager, &temp).await;
    let launch = start_in(&workspace, false);

    let checked = manager.check_launch(launch.clone(), None).await;
    let launched = manager.launch(launch, LaunchSource::Manual, None).await;

    for refused in [checked.map(drop), launched.map(drop)] {
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.is::<StoppedAgent>()),
            "{refused:?}"
        );
    }
    assert!(record.exists());
}

#[tokio::test]
async fn a_replacing_start_discards_the_stopped_agent_and_queues_its_own() {
    let (temp, manager) = manager().await;
    let (workspace, record) = stopped_workspace(&manager, &temp).await;
    let launch = start_in(&workspace, true);

    let checked = manager.check_launch(launch.clone(), None).await.unwrap();
    assert!(record.exists(), "the check discarded the stopped agent");
    let launched = manager.launch(launch, LaunchSource::Manual, None).await;

    assert_eq!(checked.map(|checked| checked.id), Some(workspace.id));
    assert!(launched.unwrap().entry.is_some());
    assert!(!record.exists());
}

#[tokio::test]
async fn a_replacing_start_that_is_refused_keeps_the_stopped_agent() {
    let (temp, manager) = manager().await;
    let (workspace, record) = stopped_workspace(&manager, &temp).await;
    let launch = Launch {
        agent: Some("missing".into()),
        ..start_in(&workspace, true)
    };

    let refused = manager.launch(launch, LaunchSource::Manual, None).await;

    assert!(refused.is_err(), "an unknown agent started");
    assert!(record.exists());
}

/// A creation on an existing workspace's branch starts no agent, so the
/// workspace's stopped agent refuses a start's check but not its own.
#[tokio::test]
async fn a_creation_check_passes_a_workspace_with_a_stopped_agent() {
    let (temp, manager) = manager().await;
    let (workspace, _record) = stopped_workspace(&manager, &temp).await;
    let branch = workspace.branch().map(ToString::to_string);
    let launch = Launch {
        target: Some(StartTarget::Repository(workspace.repository().to_string())),
        branch,
        mode: Some(crate::config::modes::ModeName::DEFAULT),
        ..Launch::default()
    };

    let created = manager.check_creation_launch(launch.clone(), None).await;
    let started = manager.check_launch(launch, None).await;

    let created = created.unwrap().map(|workspace| workspace.id);
    assert_eq!(created, Some(workspace.id));
    assert!(
        started
            .as_ref()
            .is_err_and(|error| error.is::<StoppedAgent>()),
        "{started:?}"
    );
}

#[tokio::test]
async fn a_chat_in_the_worktree_replaces_nothing() {
    let (temp, manager) = manager().await;
    let (workspace, record) = stopped_workspace(&manager, &temp).await;
    let launch = Launch {
        mode: Some("chat".into()),
        ..start_in(&workspace, true)
    };

    let refused = manager.launch(launch, LaunchSource::Manual, None).await;

    let refused = format!("{:#}", refused.expect_err("a chat replaced the agent"));
    assert!(refused.contains("--replace"), "{refused}");
    assert!(record.exists());
}

#[tokio::test]
async fn a_scoped_caller_replaces_no_stopped_agent_of_its_own_space() {
    let (temp, manager) = manager().await;
    let (workspace, record) = stopped_workspace(&manager, &temp).await;
    let caller = Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };

    let launched = start_in(&workspace, true);
    let refused = manager.launch(launched, LaunchSource::Manual, Some(&caller));

    assert!(refused.await.is_err());
    assert!(record.exists());
}
