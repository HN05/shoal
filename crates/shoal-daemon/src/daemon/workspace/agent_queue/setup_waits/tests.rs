use rusqlite::params;

use super::super::tests::{add, admitted, create, launch_in, limit_globally, preparing, register};
use crate::{
    daemon::{test_support::manager, workspace::Manager},
    model::{QueueEntry, QueueRequest, Repository, Workspace},
    protocol::operations::{OperationSpec, Preparation, QueuedLaunch},
    start_plan::QueuedStart,
    state::{QueueState, WorkspaceState},
};
use shoal_protocol::space_names::SpaceSelector;

/// The plan of a start of an agent told to fix the login in `workspace`.
fn opening(workspace: &Workspace) -> String {
    let launch = QueuedLaunch {
        prompt: Some("Fix the login".into()),
        ..QueuedLaunch::new("claude".parse().unwrap())
    };
    let request = crate::start_plan::queued_agent_request(workspace, launch, None).unwrap();
    request.plan
}

/// A start of the `plan` of a new `child` workspace of `repository`,
/// admitted to the only slot, which took its entry and then failed its
/// setup.
async fn failed_start(
    manager: &Manager,
    repository: &Repository,
    plan: fn(&Workspace) -> String,
) -> (Workspace, QueueEntry) {
    limit_globally(manager, 1);
    let child = create(manager, repository, "child").await;
    let starting = (manager.queue_add(QueueRequest {
        plan: plan(&child),
        ..launch_in(&child, false)
    }))
    .await
    .unwrap()
    .entry
    .unwrap();
    assert_eq!(starting.state, QueueState::Starting);
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

async fn plan(manager: &Manager, entry: &QueueEntry) -> QueuedStart {
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
    let (child, starting) = failed_start(&manager, &repo, opening).await;
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
    assert_eq!(waiting.opens.as_ref(), Some(&child.id));
    assert_eq!(plan.launch.prompt.as_deref(), Some("Fix the login"));
    assert!(admitted(&add(&manager, &repo, "other").await));
}

#[tokio::test]
async fn a_start_whose_setup_failed_keeps_its_entry_while_its_preparation_runs() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let (child, starting) = failed_start(&manager, &repo, opening).await;
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
        .queue_start_agent(
            (&workspace.clone().into(), true),
            launch,
            (None, Vec::new()),
        )
        .await
        .unwrap()
        .entry
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
        .queue_start_agent(
            (&workspace.clone().into(), true),
            launch,
            (None, Vec::new()),
        )
        .await
        .unwrap()
        .entry
        .unwrap();
    manager.release_setup_waits(&workspace.id).await.unwrap();
    let entries = manager.queue_entries().await.unwrap();
    let waiting = entries.iter().find(|queued| queued.id == entry.id).unwrap();
    assert_eq!(waiting.state, QueueState::Queued);
    assert!(waiting.blockers.is_empty(), "{waiting:?}");
}

#[tokio::test]
async fn a_new_workspace_ready_at_once_holds_its_agent_until_its_preparation_lets_it() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    // Created ready, as a copy with nothing to set up is.
    let workspace = manager
        .create_workspace(&repo.id, "child".into(), None, None, None)
        .await
        .unwrap();
    assert_eq!(workspace.state, WorkspaceState::Ready);
    let launch = crate::protocol::operations::QueuedLaunch::new("claude".parse().unwrap());
    let entry = manager
        .queue_start_agent(
            (&workspace.clone().into(), true),
            launch,
            (None, Vec::new()),
        )
        .await
        .unwrap()
        .entry
        .unwrap();
    assert_eq!(entry.state, QueueState::Queued);
    assert_eq!(entry.blockers, vec!["setup of child".to_owned()]);
}

#[tokio::test]
async fn a_held_start_now_is_left_to_its_holder_when_its_setup_releases_it() {
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
        .queue_start_agent(
            (&workspace.clone().into(), true),
            launch,
            (None, Vec::new()),
        )
        .await
        .unwrap()
        .entry
        .unwrap();
    manager.agent_queue.hold(&entry.id);
    manager.release_setup_waits(&workspace.id).await.unwrap();
    let entries = manager.queue_entries().await.unwrap();
    let held = entries.iter().find(|queued| queued.id == entry.id).unwrap();
    assert_eq!(held.state, QueueState::Queued);
}
