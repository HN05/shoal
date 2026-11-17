use rusqlite::params;

use super::super::tests::{add, limit_globally, register, request};
use crate::{
    daemon::{notifications::NotificationKind, test_support::manager, workspace::Manager},
    model::{QueueEntry, QueueRequest, Repository, Workspace},
    protocol::operations::{OperationSpec, Preparation},
    start_plan::{AddPlan, Creation, ResolvedAddAgent},
    state::{QueueState, WorkspaceState},
};
use shoal_protocol::space_names::SpaceSelector;

/// The plan of a start that creates `branch` in `repository`.
fn creating(repository: &Repository, branch: &str) -> String {
    let plan = AddPlan {
        repository: (&repository.id).into(),
        creation: Creation {
            branch: Some(branch.into()),
            ..Default::default()
        },
        place_name: None,
        issue: None,
        agent: ResolvedAddAgent {
            agent: Some("claude".parse().unwrap()),
            model: Default::default(),
        },
        args: Vec::new(),
        prompt: Some("Fix the login".into()),
        mode: None,
        narrowing: Default::default(),
        fork: None,
        queue_entry: None,
        chat: None,
        parent: None,
        workspace: None,
    };
    serde_json::to_string(&plan).unwrap()
}

/// A start of `plan` in `repository`, admitted to the only slot, whose new
/// `child` workspace took its entry and then failed its setup.
async fn failed_start(
    manager: &Manager,
    repository: &Repository,
    plan: String,
) -> (Workspace, QueueEntry) {
    limit_globally(manager, 1);
    let starting = (manager.queue_add(QueueRequest {
        plan,
        ..request(repository, "child", false)
    }))
    .await
    .unwrap()
    .entry
    .unwrap();
    assert_eq!(starting.state, QueueState::Starting);
    let child = manager
        .create_workspace(&repository.id, "child".into(), None, None, None)
        .await
        .unwrap();
    manager
        .queue_attach(&starting.id, &SpaceSelector::id(&child.id))
        .await
        .unwrap();
    let id = child.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET state=?2,error='setup failed',setup_finished=0 WHERE id=?1",
                params![id, WorkspaceState::Failed],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (child, starting)
}

async fn plan(manager: &Manager, entry: &QueueEntry) -> AddPlan {
    let id = entry.id.clone();
    let plan: String = (manager.store)
        .run(move |db| {
            Ok(
                db.query_row("SELECT plan FROM agent_queue WHERE id=?1", [id], |row| {
                    row.get(0)
                })?,
            )
        })
        .await
        .unwrap();
    serde_json::from_str(&plan).unwrap()
}

#[tokio::test]
async fn a_start_whose_setup_failed_waits_for_a_retried_setup_without_its_slot() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let (child, starting) = failed_start(&manager, &repo, creating(&repo, "child")).await;
    // The CLI that started it still holds the old entry.
    manager.hold_queue_entry(&starting.id).await.unwrap();
    manager.dispatch_queue().await.unwrap();

    let entries = manager.queue_entries().await.unwrap();
    let [waiting] = entries.as_slice() else {
        panic!("{entries:?}");
    };
    assert_ne!(waiting.id, starting.id);
    assert_eq!(waiting.state, QueueState::Queued);
    assert_eq!(waiting.opens.as_ref(), Some(&child.id));
    assert_eq!(waiting.blockers, ["setup of child"]);
    let plan = plan(&manager, waiting).await;
    assert!(plan.opens(&child));
    assert_eq!(plan.prompt.as_deref(), Some("Fix the login"));
    assert!(add(&manager, &repo, "other").await.start);
}

#[tokio::test]
async fn a_start_whose_setup_failed_keeps_its_entry_while_its_preparation_runs() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let (child, starting) = failed_start(&manager, &repo, creating(&repo, "child")).await;
    let spec = OperationSpec::Prepare {
        workspace: SpaceSelector::id(&child.id),
        steps: Preparation::default(),
    };
    let operation = manager.start_operation(spec, None, None).await.unwrap();
    let _worker = crate::daemon::operations::fake::take(&operation);
    manager.dispatch_queue().await.unwrap();
    let ids: Vec<_> = (manager.queue_entries().await.unwrap().into_iter())
        .map(|entry| (entry.id, entry.state))
        .collect();
    assert_eq!(ids, [(starting.id, QueueState::Starting)]);
}

#[tokio::test]
async fn a_failed_start_that_cannot_open_its_workspace_ends_and_says_so() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let (child, _) = failed_start(&manager, &repo, "{}".into()).await;
    manager.dispatch_queue().await.unwrap();
    assert!(manager.queue_entries().await.unwrap().is_empty());
    let failed: Vec<_> = (manager.notifications(false, 50).await.unwrap().into_iter())
        .filter(|notification| notification.kind == NotificationKind::QueueFailed)
        .map(|notification| notification.space_id)
        .collect();
    assert_eq!(failed, [Some(child.id)]);
}

/// A new workspace of `repository` whose preparation has yet to set it up.
async fn preparing(manager: &Manager, repository: &Repository, name: &str) -> Workspace {
    let workspace = manager
        .create_workspace(&repository.id, name.into(), None, None, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    (manager.set_state(&id, WorkspaceState::Preparing, None))
        .await
        .unwrap();
    manager.workspace_by_id(&id).await.unwrap()
}

#[tokio::test]
async fn a_start_now_waits_for_its_preparation_then_starts_past_the_limits() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    add(&manager, &repo, "first").await;
    let workspace = preparing(&manager, &repo, "child").await;
    let launch = crate::protocol::operations::QueuedLaunch {
        now: true,
        ..crate::protocol::operations::QueuedLaunch::new("claude".parse().unwrap())
    };
    let entry = manager
        .queue_for_preparation(&workspace, launch)
        .await
        .unwrap();
    assert_eq!(entry.state, QueueState::Queued);
    assert_eq!(entry.blockers, vec!["setup of child".to_owned()]);
    manager.release_setup_waits(&workspace.id).await.unwrap();
    let entries = manager.queue_entries().await.unwrap();
    let started = entries.iter().find(|queued| queued.id == entry.id).unwrap();
    assert_eq!(started.state, QueueState::Starting);
}

#[tokio::test]
async fn a_queued_start_waits_for_its_preparation_then_for_the_limits() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    add(&manager, &repo, "first").await;
    let workspace = preparing(&manager, &repo, "child").await;
    let launch = crate::protocol::operations::QueuedLaunch::new("claude".parse().unwrap());
    let entry = manager
        .queue_for_preparation(&workspace, launch)
        .await
        .unwrap();
    manager.release_setup_waits(&workspace.id).await.unwrap();
    let entries = manager.queue_entries().await.unwrap();
    let waiting = entries.iter().find(|queued| queued.id == entry.id).unwrap();
    assert_eq!(waiting.state, QueueState::Queued);
    assert!(waiting.blockers.is_empty(), "{waiting:?}");
}
