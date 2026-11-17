use std::{sync::Arc, time::Duration};

use super::{Opening, Preparing};
use crate::{
    daemon::notifications::NotificationKind,
    daemon::{
        operations::fake,
        test_support::{git, manager, repository},
        workspace::Manager,
    },
    model::{Repository, Workspace},
    protocol::operations::{
        Ended, Failure, OperationSpec, Report, StartedWorkspace, WorkspaceStart,
    },
    removal::{InspectionPolicy, UnsavedWork},
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;

/// A generous guard against a hang; no test waits this long when it passes.
const HANG: Duration = Duration::from_secs(60);

async fn registered(root: &std::path::Path, manager: &Manager) -> Repository {
    let path = repository(root, "repo");
    manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

/// Start a new workspace on branch `name` from `base`, without an agent.
async fn start(
    manager: &Arc<Manager>,
    repository: &Repository,
    name: &str,
    base: &str,
) -> StartedWorkspace {
    let opening = Opening {
        start: WorkspaceStart::Name(name.into()),
        base: Some(base.into()),
        new: Default::default(),
        parent: None,
    };
    let preparing = Preparing {
        issue: None,
        pr: None,
        agent: None,
        waiting: None,
        queue_entry: None,
        env: None,
    };
    manager
        .start_workspace((&repository.id).into(), opening, preparing, None)
        .await
        .unwrap()
}

async fn read(manager: &Manager, workspace: &Workspace) -> Workspace {
    manager.workspace_by_id(&workspace.id).await.unwrap()
}

#[tokio::test]
async fn a_start_records_the_workspace_and_leaves_its_worktree_to_its_preparation() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let started = start(&manager, &repository, "child", "HEAD").await;
    let workspace = *started.workspace;
    assert!(workspace.pending_worktree);
    assert_eq!(workspace.state, WorkspaceState::Preparing);
    assert!(!workspace.path.exists());
    let worker = fake::take(&started.operation);
    let OperationSpec::Prepare { steps, .. } = &worker.order.spec else {
        panic!("expected a preparation: {:?}", worker.order);
    };
    assert!(steps.setup);
    // The preparation creates it first.
    let selector = SpaceSelector::id(&workspace.id);
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert!(!created.pending_worktree);
    assert!(created.path.join("tracked").exists());
    assert_eq!(
        git(&created.path, &["branch", "--show-current"]).trim(),
        "child"
    );
}

#[tokio::test]
async fn a_failed_worktree_stays_pending_for_a_retried_preparation() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let started = start(&manager, &repository, "child", "later").await;
    let _worker = fake::take(&started.operation);
    let selector = SpaceSelector::id(&started.workspace.id);
    let failed = manager.create_pending_worktree(&selector).await;
    assert!(failed.is_err(), "{failed:?}");
    let workspace = read(&manager, &started.workspace).await;
    assert_eq!(workspace.state, WorkspaceState::Failed);
    assert!(workspace.pending_worktree);
    // Once its base exists, a retry creates it.
    git(&repository.path, &["branch", "later"]);
    let created = manager.create_pending_worktree(&selector).await.unwrap();
    assert!(!created.pending_worktree);
    assert!(created.path.exists());
    let again = manager.create_pending_worktree(&selector).await;
    assert!(again.is_err(), "a second creation ran: {again:?}");
}

#[tokio::test]
async fn a_workspace_without_its_worktree_yet_goes_with_its_record() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let started = start(&manager, &repository, "child", "HEAD").await;
    let _worker = fake::take(&started.operation);
    let selector = SpaceSelector::id(&started.workspace.id);
    manager
        .remove_selected(&selector, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert!(manager.workspace(&selector).await.is_err());
    // Its branch was never created, so no removal kept it.
    let branches = git(&repository.path, &["branch", "--list", "child"]);
    assert_eq!(branches.trim(), "");
}

#[tokio::test]
async fn a_workspace_whose_worktree_failed_goes_with_its_record() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let started = start(&manager, &repository, "child", "later").await;
    let _worker = fake::take(&started.operation);
    let selector = SpaceSelector::id(&started.workspace.id);
    let failed = manager.create_pending_worktree(&selector).await;
    assert!(failed.is_err(), "{failed:?}");
    manager
        .remove_selected(&selector, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert!(manager.workspace(&selector).await.is_err());
}

#[tokio::test]
async fn a_failed_preparation_reaches_the_user_with_what_retries_it() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let started = start(&manager, &repository, "child", "HEAD").await;
    let worker = fake::take(&started.operation);
    let message = "fatal: no space left".to_owned();
    let failed = Ended::Failed(Failure::WorktreeFailed { message });
    (manager.report_operation(&started.operation, Report::Ended(failed), None))
        .await
        .unwrap();
    worker.exit(0);
    let number = started.workspace.number;
    let notification = tokio::time::timeout(HANG, async {
        loop {
            let notifications = manager.notifications(false, 10).await.unwrap();
            let found = (notifications.into_iter())
                .find(|notification| notification.kind == NotificationKind::PreparationFailed);
            match found {
                Some(found) => return found,
                None => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .expect("the failure was reported");
    assert_eq!(notification.space_id.as_ref(), Some(&started.workspace.id));
    assert!(
        notification.message.contains("fatal: no space left"),
        "{}",
        notification.message
    );
    assert!(
        notification
            .message
            .contains(&format!("`shoal restore {number}`")),
        "{}",
        notification.message
    );
}
