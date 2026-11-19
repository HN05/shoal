use crate::daemon::manager::Manager;
use crate::daemon::test_support::{global_config, manager, repository};
use shoal_protocol::launches::{IssueChoice, Launch, LaunchSource, StartTarget};

/// A registered repository whose remote is a forge's, which Shoal never
/// reaches, and an agent that starts without limits.
async fn offline_machine(manager: &Manager, temp: &tempfile::TempDir) -> crate::model::Repository {
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 'none'\n[machine.overload.disk]\nenabled = false\n\
         [agents.claude]\ncommand = ['sh']\n",
    ));
    let path = repository(temp.path(), "repo");
    crate::daemon::test_support::git(&path, &["config", "protocol.allow", "never"]);
    let remote = ["remote", "add", "origin", "https://github.com/team/repo"];
    crate::daemon::test_support::git(&path, &remote);
    let path = path.to_str().unwrap().into();
    manager.register_repository(path, None, None).await.unwrap()
}

/// The launch of a workspace named `fix` in `repo` on issue 316, which
/// nothing reads, on `branch` when given.
fn launch_on_issue(repo: &crate::model::Repository, branch: Option<&str>) -> Launch {
    Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        name: Some("fix".into()),
        branch: branch.map(Into::into),
        agent: Some("claude".into()),
        issue: Some(IssueChoice::Named("316".parse().unwrap())),
        ..Launch::default()
    }
}

/// A workspace on an issue its launch cannot read is refused before
/// anything is recorded, even one the launch names and gives its branch,
/// which needs no title from the issue.
#[tokio::test]
async fn a_workspace_on_an_unread_issue_is_refused() {
    let (temp, manager) = manager().await;
    let repo = offline_machine(&manager, &temp).await;

    for branch in [Some("fix-316"), None] {
        let launch = launch_on_issue(&repo, branch);
        let refused = (manager.launch(launch, LaunchSource::Manual, None)).await;

        assert!(refused.is_err());
    }
    assert!(manager.list_workspaces().await.unwrap().is_empty());
    assert!(manager.queue_entries().await.unwrap().is_empty());
}
