use std::{sync::Arc, time::Duration};

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    paths::Paths,
    protocol::operations::{
        Ended, Failure, OperationSpec, Outcome, Preparation, Prepared, Progress, Step, Stream,
    },
    space_names::SpaceSelector,
};

use super::{Entry, Operations, RETAIN, fake};

/// A generous guard against a hang; no test waits this long when it passes.
const HANG: Duration = Duration::from_secs(60);

/// The operations of a daemon whose workers are fakes.
fn operations(root: &tempfile::TempDir) -> Arc<Operations> {
    let operations = Operations::new(Paths::for_test(root.path()), Arc::default());
    operations.fake_workers();
    Arc::new(operations)
}

fn setup(workspace: &SpaceId) -> OperationSpec {
    OperationSpec::Prepare {
        workspace: SpaceSelector::id(workspace),
        steps: Preparation {
            setup: true,
            ..Default::default()
        },
    }
}

/// Start the setup of a new workspace, returning the workspace, the
/// operation's ID and its entry.
async fn started(operations: &Arc<Operations>) -> (SpaceId, String, Arc<Entry>) {
    let workspace = SpaceId::generate();
    let spec = setup(&workspace);
    let id = operations
        .spawn(workspace.clone(), spec, None, None)
        .await
        .unwrap();
    let entry = operations.get(&id).unwrap();
    (workspace, id, entry)
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

fn output(stream: Stream, line: &str) -> Progress {
    Progress::Output {
        stream,
        line: line.into(),
    }
}

#[tokio::test]
async fn the_reported_end_takes_effect_once_the_worker_exits_after_its_output() {
    let root = tempfile::tempdir().unwrap();
    let operations = operations(&root);
    let (workspace, id, entry) = started(&operations).await;
    let mut worker = fake::take(&id);
    let OperationSpec::Prepare {
        workspace: id_in_order,
        ..
    } = &worker.order.spec
    else {
        panic!("expected the setup order: {:?}", worker.order);
    };
    assert_eq!(*id_in_order, SpaceSelector::id(&workspace));
    assert_eq!(worker.token, None);
    entry.report_step(Step::SetupStarted);
    worker.stdout("installing").await;
    worker.stderr("warning: slow\r").await;
    until(&entry, |progress, _| progress.len() == 3).await;
    let workspace = shoal_protocol::test_support::workspace("api", "repo", "api", root.path());
    let prepared = Prepared {
        workspace: Box::new(workspace),
        agent: None,
    };
    entry.report_end(Ended::Succeeded(Box::new(Outcome::Prepared(prepared))));
    assert!(
        entry.events_from(0).1.is_none(),
        "the worker has not exited"
    );
    worker.exit(0);
    until(&entry, |_, ended| ended).await;
    let (events, ended) = entry.events_from(0);
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
    let (later, _) = entry.events_from(2);
    assert_eq!(later.len(), 1, "a follower resumes after what it read");
}

#[tokio::test]
async fn an_interrupted_worker_that_exits_without_a_report_fails_the_operation() {
    let root = tempfile::tempdir().unwrap();
    let operations = operations(&root);
    let (_, id, entry) = started(&operations).await;
    let worker = fake::take(&id);
    entry.interrupt();
    assert!(worker.interrupted.load(std::sync::atomic::Ordering::SeqCst));
    worker.exit(3);
    until(&entry, |_, ended| ended).await;
    let Some(Ended::Failed(Failure::Failed { message })) = entry.events_from(0).1 else {
        panic!("expected a failure");
    };
    assert!(message.contains("status 3"), "{message}");
}

#[tokio::test]
async fn an_ended_operation_is_forgotten_once_its_retention_passes_without_a_new_one() {
    let root = tempfile::tempdir().unwrap();
    let operations = operations(&root);
    let (_, id, entry) = started(&operations).await;
    let mut worker = fake::take(&id);
    worker.stdout("installing").await;
    until(&entry, |progress, _| progress.len() == 1).await;
    tokio::time::pause();
    worker.exit(0);
    until(&entry, |_, ended| ended).await;
    tokio::time::sleep(RETAIN - Duration::from_secs(1)).await;
    assert!(operations.get(&id).is_some(), "still retained");
    tokio::time::sleep(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert!(operations.get(&id).is_none(), "forgotten");
    let (events, ended) = entry.events_from(0);
    assert!(ended.is_some(), "a follower keeps what it holds");
    assert!(
        events
            .iter()
            .any(|event| event.progress == output(Stream::Stdout, "installing"))
    );
}

#[tokio::test]
async fn a_long_line_arrives_in_parts_of_bounded_size() {
    let root = tempfile::tempdir().unwrap();
    let operations = operations(&root);
    let (_, id, entry) = started(&operations).await;
    let mut worker = fake::take(&id);
    worker.stdout(&"x".repeat(2 * 8 * 1024 + 10)).await;
    worker.exit(0);
    until(&entry, |_, ended| ended).await;
    let lengths: Vec<_> = entry
        .events_from(0)
        .0
        .into_iter()
        .map(|event| match event.progress {
            Progress::Output { line, .. } => line.len(),
            progress => panic!("expected output: {progress:?}"),
        })
        .collect();
    assert_eq!(lengths, [8 * 1024, 8 * 1024, 10]);
}
