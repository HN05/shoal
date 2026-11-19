use super::super::tests::offline_machine;
use crate::daemon::test_support::manager;
use shoal_protocol::launches::{Launch, LaunchSource, StartTarget};

/// The launch of a workspace in `repo` on `branch`, as a start's pickers
/// leave it.
fn launch_on_branch(repo: &crate::model::Repository, branch: Option<&str>) -> Launch {
    Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        branch: branch.map(Into::into),
        ..Launch::default()
    }
}

/// A check of a launch on the branch a workspace has answers with that
/// workspace, which the start goes into, and refuses what only a new
/// workspace takes, as the start does.
#[tokio::test]
async fn a_check_answers_the_workspace_that_has_the_branch() {
    let (temp, manager) = manager().await;
    let repo = offline_machine(&manager, &temp).await;
    let base = Some("main".into());
    let fix = (manager.create_workspace(&repo.id, "fix".into(), base, None, None))
        .await
        .unwrap();
    let named = Launch {
        name: Some("other".into()),
        ..launch_on_branch(&repo, Some("fix"))
    };

    let checked = manager.check_workspace_launch(launch_on_branch(&repo, Some("fix")), None);

    assert_eq!(
        checked.await.unwrap().map(|workspace| workspace.id),
        Some(fix.id)
    );
    assert!(manager.check_workspace_launch(named, None).await.is_err());
}

/// A check refuses what creating its workspace would refuse, and creates
/// nothing.
#[tokio::test]
async fn a_check_refuses_what_creation_would_and_creates_nothing() {
    let (temp, manager) = manager().await;
    let repo = offline_machine(&manager, &temp).await;
    let launch = Launch {
        git_profile: Some("missing".into()),
        ..launch_on_branch(&repo, Some("fix"))
    };

    assert!(manager.check_workspace_launch(launch, None).await.is_err());
    assert!(manager.list_workspaces().await.unwrap().is_empty());
}

/// A check leaves open what a picker has yet to choose: a launch that
/// names only a base, which its start would begin as a chat and refuse,
/// passes until a branch is picked.
#[tokio::test]
async fn a_check_leaves_the_branch_to_a_picker() {
    let (temp, manager) = manager().await;
    let repo = offline_machine(&manager, &temp).await;
    let launch = Launch {
        base: Some("main".into()),
        agent: Some("claude".into()),
        ..launch_on_branch(&repo, None)
    };

    let checked = manager.check_workspace_launch(launch.clone(), None).await;

    assert!(checked.unwrap().is_none());
    let started = manager.launch(launch, LaunchSource::Manual, None).await;
    assert!(format!("{:#}", started.unwrap_err()).contains("--base"));
}
