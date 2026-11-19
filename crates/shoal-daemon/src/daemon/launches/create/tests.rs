use crate::daemon::test_support::{global_config, manager, repository};
use crate::daemon::workspace::Manager;
use shoal_protocol::launches::{Launch, StartTarget};

/// A registered repository and an agent that starts without limits, which
/// `agent.default` names.
async fn machine(manager: &Manager, temp: &tempfile::TempDir) -> crate::model::Repository {
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 'none'\n[machine.overload.disk]\nenabled = false\n\
         [agent]\ndefault = 'claude'\n[agents.claude]\ncommand = ['sh']\n",
    ));
    let path = repository(temp.path(), "repo");
    let path = path.to_str().unwrap().into();
    manager.register_repository(path, None, None).await.unwrap()
}

/// A creation in `repo` on `branch`.
fn on_branch(repo: &crate::model::Repository, branch: &str) -> Launch {
    Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        branch: Some(branch.into()),
        ..Launch::default()
    }
}

/// A creation records a workspace on the branch it names, at the top
/// without a parent, and queues no agent there, though `agent.default`
/// names one.
#[tokio::test]
async fn a_creation_records_a_top_level_workspace_without_an_agent() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp).await;

    let started = (manager.create_placed_workspace(on_branch(&repo, "fix"), (None, None), None))
        .await
        .unwrap();

    assert_eq!(started.workspace.branch().map(|b| b.as_str()), Some("fix"));
    assert_eq!(started.workspace.parent, None);
    assert!(started.entry.is_none());
    assert!(manager.queue_entries().await.unwrap().is_empty());
}

/// A creation that names what only an agent takes, or nothing a branch
/// comes from, is refused before anything is recorded.
#[tokio::test]
async fn a_creation_refuses_an_agent_and_a_start_without_a_branch() {
    let (temp, manager) = manager().await;
    let repo = machine(&manager, &temp).await;
    let with_agent = Launch {
        agent: Some("claude".into()),
        ..on_branch(&repo, "fix")
    };
    let without_branch = Launch {
        branch: None,
        ..on_branch(&repo, "fix")
    };

    for launch in [with_agent, without_branch] {
        assert!(
            manager
                .create_placed_workspace(launch, (None, None), None)
                .await
                .is_err()
        );
    }
    assert!(manager.list_workspaces().await.unwrap().is_empty());
}
