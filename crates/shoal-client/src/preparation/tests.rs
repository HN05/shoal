use crate::preparation::{self, NewChild, NewWorkspace};
use crate::test_daemon::Connect as _;
use shoal_daemon::test_daemon::{HANG, TestDaemon, fake};
use shoal_protocol::protocol::operations::{Ended, Failure, Progress, Report, WorkspaceStart};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::state::WorkspaceState;

#[tokio::test]
async fn an_app_creates_a_workspace_and_follows_its_setup_to_a_typed_failure() {
    let daemon = TestDaemon::start().await;
    let app = daemon.connector();
    let fail = async |operation: &str, failure| {
        let report = Report::Ended(Ended::Failed(failure));
        let manager = &daemon.manager;
        manager
            .report_operation(operation, report, None)
            .await
            .unwrap();
    };
    let repository = daemon.repository.id.clone().into();
    let new = NewWorkspace {
        base: Some("HEAD".into()),
        ..NewWorkspace::new(repository, WorkspaceStart::Name("api".into()))
    };
    let preparing = preparation::create(&app, new, Default::default())
        .await
        .unwrap();
    let mut worker = fake::take(&preparing.operation().id);
    worker.stdout("installing").await;
    // A failed setup command leaves the workspace failed.
    (daemon.manager)
        .spaces
        .set_state(&preparing.workspace().id, WorkspaceState::Failed, None)
        .await
        .unwrap();
    let message = "setup command exited with status 2".to_owned();
    fail(&preparing.operation().id, Failure::SetupFailed { message }).await;
    worker.exit(0);
    let mut lines = Vec::new();
    let progress = |progress| match progress {
        Progress::Output { line, .. } => lines.push(line),
        Progress::Step { .. } => {}
    };
    let finished = tokio::time::timeout(HANG, preparing.finish(progress));
    let unprepared = finished.await.unwrap().unwrap().unwrap_err();
    assert_eq!(lines, ["installing"]);
    assert_eq!(unprepared.workspace.name, "api");
    assert!(matches!(unprepared.failure, Failure::SetupFailed { .. }));
    assert!(!unprepared.set_up(), "setup awaits a retry");
    // A split whose post-setup hook fails after setup left it ready.
    let parent = daemon.workspace("parent").await;
    let split = NewChild {
        base: Some("HEAD".into()),
        ..NewChild::new(SpaceSelector::id(&parent.id), Some("child".into()))
    };
    let preparing = preparation::create(&app, split, Default::default())
        .await
        .unwrap();
    let worker = fake::take(&preparing.operation().id);
    let child = preparing.workspace().id.clone();
    (daemon.manager)
        .spaces
        .set_state(&child, WorkspaceState::Ready, None)
        .await
        .unwrap();
    let message = "post_setup exited with 1".to_owned();
    fail(&preparing.operation().id, Failure::HookFailed { message }).await;
    worker.exit(0);
    let finished = tokio::time::timeout(HANG, preparing.finish(|_| {}));
    let unprepared = finished.await.unwrap().unwrap().unwrap_err();
    assert_eq!(unprepared.workspace.id, child);
    assert!(unprepared.set_up(), "the hook failed after setup");
}
