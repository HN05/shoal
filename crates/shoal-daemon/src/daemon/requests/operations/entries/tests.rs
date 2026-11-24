use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::time::Duration;

use super::{named_operation, operation, report};
use crate::{
    daemon::{
        manager::Manager,
        operations::{Entry, fake},
        test_support::{manager, repository},
    },
    model::{Repository, Workspace},
    protocol::operations::{OperationRef, OperationSpec, Preparation, Progress, Report, Step},
    refusal::{Reason, Refusal},
};
use shoal_protocol::space_names::SpaceSelector;

/// A generous guard against a hang; no test waits this long when it passes.
const HANG: Duration = Duration::from_secs(60);

async fn workspace(manager: &Manager, repository: &Repository, name: &str) -> Workspace {
    manager
        .create_workspace(&repository.id, name.into(), Some("HEAD".into()), None, None)
        .await
        .unwrap()
}

async fn registered(root: &std::path::Path, manager: &Manager) -> Repository {
    let path = repository(root, "repo");
    manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

fn setup(workspace: &Workspace) -> OperationSpec {
    OperationSpec::Prepare {
        workspace: SpaceSelector::id(&workspace.id),
        steps: Preparation {
            setup: true,
            ..Default::default()
        },
    }
}

/// Wait until `entry`'s events and end satisfy `done`.
async fn until(entry: &Entry, done: impl Fn(&[Progress], bool) -> bool) {
    let mut changed = entry.watch();
    tokio::time::timeout(HANG, async {
        loop {
            changed.borrow_and_update();
            let (events, ended) = entry.events_from(0);
            let progress: Vec<_> = events.into_iter().map(|event| event.progress).collect();
            if done(&progress, ended.is_some()) {
                return;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("the operation got there");
}

#[tokio::test]
async fn the_worker_acts_with_the_callers_scope_which_follows_only_its_tree() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let own = workspace(&manager, &repository, "own").await;
    let other = workspace(&manager, &repository, "other").await;
    let token = &manager
        .spaces
        .issue_scope(&SpaceSelector::id(&own.id))
        .await
        .unwrap();
    let caller = manager
        .coordination
        .caller(&*manager, token)
        .await
        .unwrap()
        .unwrap();
    let id = manager
        .spaces
        .start_operation(setup(&own), None, Some(&caller))
        .await
        .unwrap();
    // The worker gets a token of its own for the caller's workspace, which
    // outlives revoking the caller's and goes once the worker exits.
    let worker = fake::take(&id);
    let issued = worker.token.clone().unwrap();
    assert_ne!(&issued, token);
    manager
        .spaces
        .revoke_scope(&SpaceSelector::id(&own.id), token.clone())
        .await
        .unwrap();
    let acting = manager
        .coordination
        .caller(&*manager, &issued)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acting.space_id, own.id);
    let entry = operation(&manager, &id, Some(&caller)).await.unwrap();
    worker.exit(0);
    until(&entry, |_, ended| ended).await;
    assert!(
        manager
            .coordination
            .caller(&*manager, &issued)
            .await
            .unwrap()
            .is_none()
    );
    // A worker started from an execution acts as that execution even once
    // it ended, as when a `shoal run` command goes before its operation.
    let ended = crate::daemon::scope::Caller {
        space_id: own.id.clone(),
        execution: Some(shoal_coordination::scope::ScopedExecution {
            id: "ended".into(),
            kind: crate::daemon::executions::ExecutionKind::Command,
            agent_execution: Some("agent".into()),
        }),
        granted: None,
        read_only: false,
        vouched: None,
    };
    let from_execution = manager
        .spaces
        .start_operation(setup(&own), None, Some(&ended))
        .await
        .unwrap();
    let worker = fake::take(&from_execution);
    let acting = manager
        .coordination
        .caller(&*manager, worker.token.as_ref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        acting.execution_id().map(ExecutionId::as_str),
        Some("ended")
    );
    worker.exit(0);
    let theirs = manager
        .spaces
        .start_operation(setup(&other), None, None)
        .await
        .unwrap();
    fake::take(&theirs);
    let refused = operation(&manager, &theirs, Some(&caller)).await;
    assert!(
        refused.is_err(),
        "another workspace's operation was followed"
    );
    let missing = operation(&manager, "missing", None).await.err().unwrap();
    assert_eq!(
        crate::refusal::Reason::of(&missing),
        Some(crate::refusal::Reason::NotFound)
    );
}

#[tokio::test]
async fn an_operation_named_with_its_space_is_found_here_after_its_space_left() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let other = workspace(&manager, &repository, "api").await;
    // A move's operation outlives the space it moved away.
    let gone = SpaceId::generate();
    let spec = setup(&other);
    let id = (manager.operations.spawn(gone.clone(), spec, None, None))
        .await
        .unwrap();
    fake::take(&id);
    let named = OperationRef::in_space(id.clone(), SpaceSelector::id(&gone));
    let entry = named_operation(&manager, &named, None).await.unwrap();
    assert_eq!(entry.workspace, gone);
    // Named with another space, it is not that space's.
    let elsewhere = OperationRef::in_space(id, SpaceSelector::id(&other.id));
    let Err(refused) = named_operation(&manager, &elsewhere, None).await else {
        panic!("another space's operation was found");
    };
    assert_eq!(Reason::of(&refused), Some(Reason::NotFound), "{refused:#}");
    // One this machine does not run is refused as the absent space's, which
    // that space's machine answers instead.
    let there = OperationRef::in_space("unknown".into(), SpaceSelector::id(&gone));
    let Err(refused) = named_operation(&manager, &there, None).await else {
        panic!("an unknown operation was found");
    };
    let absent = Refusal::absent_space(&refused);
    assert_eq!(absent, Some(&SpaceSelector::id(&gone)), "{refused:#}");
}

#[tokio::test]
async fn a_scoped_caller_follows_and_reports_its_own_operation_in_a_space_no_longer_here() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let own = workspace(&manager, &repository, "own").await;
    let other = workspace(&manager, &repository, "other").await;
    let caller = |space: &Workspace| {
        let manager = &manager;
        let space = SpaceSelector::id(&space.id);
        async move {
            let token = manager.spaces.issue_scope(&space).await.unwrap();
            manager
                .coordination
                .caller(&**manager, &token)
                .await
                .unwrap()
                .unwrap()
        }
    };
    let (starter, outsider) = (caller(&own).await, caller(&other).await);
    // As a move's, once the space it moved left this machine and before
    // the hub lists it at its destination, below the starter.
    let gone = SpaceId::generate();
    let id = manager
        .operations
        .spawn(gone, setup(&own), None, Some(&starter))
        .await
        .unwrap();
    let worker = fake::take(&id);
    let acting = manager
        .coordination
        .caller(&*manager, worker.token.as_ref().unwrap())
        .await
        .unwrap()
        .unwrap();

    let followed = operation(&manager, &id, Some(&starter)).await;
    let reported = report(
        &manager,
        &id,
        Report::Step(Step::SetupStarted),
        Some(&acting),
    )
    .await;
    let refused = operation(&manager, &id, Some(&outsider)).await;

    followed.unwrap();
    reported.unwrap();
    let Err(refused) = refused else {
        panic!("another space followed it");
    };
    assert!(
        refused.to_string().contains("can only follow"),
        "{refused:#}"
    );
    worker.exit(0);
}
