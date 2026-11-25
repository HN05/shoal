use rusqlite::params;

use crate::daemon::agent_service::tests::queue::{create, launch_in, limit_globally, register};
use crate::{
    daemon::{manager::Manager, test_support::manager},
    model::{QueueEntry, QueueRequest, Repository, Workspace},
    protocol::operations::{OperationSpec, Preparation, QueuedLaunch},
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
    let starting = (manager.agents.queue_add(
        manager,
        QueueRequest {
            plan: plan(&child),
            ..launch_in(&child, false)
        },
    ))
    .await
    .unwrap()
    .entry
    .unwrap();
    assert_eq!(starting.state, QueueState::Starting);
    manager
        .agents
        .queue_attach(manager, &starting.id, &SpaceSelector::id(&child.id))
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

#[tokio::test]
async fn a_start_whose_setup_failed_keeps_its_entry_while_its_preparation_runs() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let (child, starting) = failed_start(&manager, &repo, opening).await;
    let spec = OperationSpec::Prepare {
        workspace: SpaceSelector::id(&child.id),
        steps: Preparation::default(),
    };
    let operation = manager
        .spaces
        .start_operation(spec, None, None)
        .await
        .unwrap();
    let _worker = crate::daemon::operations::fake::take(&operation);
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let ids: Vec<_> = (manager
        .agents
        .queue_entries(&*manager)
        .await
        .unwrap()
        .into_iter())
    .map(|entry| (entry.id, entry.state))
    .collect();
    assert_eq!(ids, [(starting.id, QueueState::Starting)]);
}
