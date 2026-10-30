use std::time::Duration;

use super::{Entry, fake};
use crate::{
    daemon::workspace::Manager,
    model::{Repository, Workspace},
    protocol::operations::{
        Ended, Failure, OperationSpec, Outcome, Preparation, Prepared, Progress, Report, Step,
        Stream,
    },
    test_support::{manager, repository},
};

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
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

fn setup(workspace: &Workspace) -> OperationSpec {
    OperationSpec::Prepare {
        workspace: workspace.name.clone(),
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
            let (events, ended) = entry.since(0);
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

fn output(stream: Stream, line: &str) -> Progress {
    Progress::Output {
        stream,
        line: line.into(),
    }
}

#[tokio::test]
async fn the_reported_end_takes_effect_once_the_worker_exits_after_its_output() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let workspace = workspace(&manager, &repository, "api").await;
    let id = manager
        .start_operation(setup(&workspace), None, None)
        .await
        .unwrap();
    let mut worker = fake::take(&id);
    let OperationSpec::Prepare {
        workspace: id_in_order,
        ..
    } = &worker.order.spec;
    assert_eq!(id_in_order, &workspace.id);
    assert_eq!(worker.token, None);
    let entry = manager.operation(&id, None).await.unwrap();
    let step = Report::Step(Step::SetupStarted);
    manager.report_operation(&id, step, None).await.unwrap();
    worker.stdout("installing").await;
    worker.stderr("warning: slow\r").await;
    until(&entry, |progress, _| progress.len() == 3).await;
    let prepared = Prepared {
        workspace: Box::new(workspace.clone()),
        agent: None,
    };
    let ended = Ended::Succeeded(Box::new(Outcome::Prepared(prepared)));
    let report = Report::Ended(ended);
    manager.report_operation(&id, report, None).await.unwrap();
    assert!(entry.since(0).1.is_none(), "the worker has not exited");
    worker.exit(0);
    until(&entry, |_, ended| ended).await;
    let (events, ended) = entry.since(0);
    let progress: Vec<_> = events.into_iter().map(|event| event.progress).collect();
    assert_eq!(
        progress[0],
        Progress::Step {
            step: Step::SetupStarted
        }
    );
    assert!(progress.contains(&output(Stream::Stdout, "installing")));
    assert!(progress.contains(&output(Stream::Stderr, "warning: slow")));
    assert!(matches!(
        ended,
        Some(Ended::Succeeded(outcome)) if matches!(*outcome, Outcome::Prepared(_))
    ));
    let (later, _) = entry.since(2);
    assert_eq!(later.len(), 1, "a follower resumes after what it read");
}

#[tokio::test]
async fn an_interrupted_worker_that_exits_without_a_report_fails_the_operation() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let workspace = workspace(&manager, &repository, "api").await;
    let id = manager
        .start_operation(setup(&workspace), None, None)
        .await
        .unwrap();
    let worker = fake::take(&id);
    let entry = manager.operation(&id, None).await.unwrap();
    entry.interrupt();
    assert!(worker.interrupted.load(std::sync::atomic::Ordering::SeqCst));
    worker.exit(3);
    until(&entry, |_, ended| ended).await;
    let Some(Ended::Failed(Failure::Failed { message })) = entry.since(0).1 else {
        panic!("expected a failure");
    };
    assert!(message.contains("status 3"), "{message}");
}

#[tokio::test]
async fn the_worker_acts_with_the_callers_scope_which_follows_only_its_tree() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let own = workspace(&manager, &repository, "own").await;
    let other = workspace(&manager, &repository, "other").await;
    let environment = manager.workspace_environment(&own.id).await.unwrap();
    let token = &environment[crate::env::SCOPE_TOKEN];
    let caller = manager.caller(token).await.unwrap().unwrap();
    let id = manager
        .start_operation(setup(&own), None, Some(&caller))
        .await
        .unwrap();
    assert_eq!(fake::take(&id).token.as_ref(), Some(token));
    manager.operation(&id, Some(&caller)).await.unwrap();
    let theirs = manager
        .start_operation(setup(&other), None, None)
        .await
        .unwrap();
    fake::take(&theirs);
    let refused = manager.operation(&theirs, Some(&caller)).await;
    assert!(
        refused.is_err(),
        "another workspace's operation was followed"
    );
    let missing = manager.operation("missing", None).await.err().unwrap();
    assert_eq!(
        crate::daemon::refusal::Reason::of(&missing),
        Some(crate::daemon::refusal::Reason::NotFound)
    );
}
