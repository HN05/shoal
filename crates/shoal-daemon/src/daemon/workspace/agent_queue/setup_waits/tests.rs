use rusqlite::params;

use super::super::tests::{add, limit_globally, register, request};
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{notifications::NotificationKind, test_support::manager, workspace::Manager},
    model::{QueueEntry, QueueRequest, Repository, Workspace},
    protocol::operations::{OperationSpec, Preparation},
    start_plan::{AddPlan, Creation, ResolvedAddAgent},
    state::{QueueState, WorkspaceState},
};

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
