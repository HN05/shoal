use crate::daemon::space_service::tests::launches_workspace::offline_machine;
use crate::daemon::test_support::manager;
use shoal_protocol::launches::{Launch, StartTarget};

/// The launch of a workspace in `repo` on `branch`, as a start's pickers
/// leave it.
fn launch_on_branch(repo: &crate::model::Repository, branch: Option<&str>) -> Launch {
    Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        branch: branch.map(Into::into),
        ..Launch::default()
    }
}

/// A check refuses a blocker the daemon knows nothing of, whether its
/// launch goes into the workspace that has its branch or a new one.
#[tokio::test]
async fn a_check_refuses_an_unknown_blocker_on_either_branch() {
    let (temp, manager) = manager().await;
    let repo = offline_machine(&manager, &temp).await;
    let base = Some("main".into());
    (manager.create_workspace(&repo.id, "fix".into(), base, None, None))
        .await
        .unwrap();
    let blocked = |branch: &str| Launch {
        blockers: vec!["missing".into()],
        ..launch_on_branch(&repo, Some(branch))
    };

    for branch in ["fix", "new"] {
        let checked = manager
            .spaces
            .check_launch(&*manager, blocked(branch), None)
            .await;
        let error = format!("{:#}", checked.unwrap_err());
        assert!(error.contains("missing"), "{branch}: {error}");
    }
}
