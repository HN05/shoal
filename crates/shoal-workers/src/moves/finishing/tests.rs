use super::*;
use crate::protocol::operations::Step;
use shoal_core::transfer::{Carrying, SshDestination};
use shoal_daemon::test_daemon::TestDaemon;
use shoal_protocol::removal::UnsavedWork;

/// Run this module's helper `name` in a process of its own, without the
/// runner's Shoal variables, which would reach the daemon in its scope.
async fn isolated(name: &str) {
    let root = tempfile::tempdir().unwrap();
    let name = format!("moves::finishing::tests::{name}");
    let output = (crate::test_support::isolated_test(root.path(), &name).output())
        .await
        .unwrap();
    let printed = crate::test_support::printed(&output);
    assert!(output.status.success(), "{printed}");
}

/// Whether this process is a helper [`isolated`] runs.
fn helper() -> bool {
    std::env::var_os("SHOAL_TEST_HELPER").is_some()
}

/// Steps no one follows.
struct Unfollowed;

impl Steps for Unfollowed {
    async fn reached(&self, _step: Step) {}

    fn ensure_running(&self) -> Result<()> {
        Ok(())
    }
}

/// A daemon with a workspace whose move to `destination` wrote its package
/// and was recorded in `phase`, as its worker left it, with the paths that
/// reach the daemon.
async fn left_move(
    daemon: &TestDaemon,
    destination: impl FnOnce(&std::path::Path) -> MoveDestination,
) -> (CommandPaths, MoveRecord, std::path::PathBuf) {
    let workspace = daemon.workspace("moving").await;
    let paths = CommandPaths::from(daemon.manager.paths.clone());
    let destination = destination(&daemon.manager.paths.home);
    let space = SpaceSelector::id(&workspace.id);
    let (unsaved, carrying) = (UnsavedWork::default(), Carrying::Subtree);
    let packing = crate::client::transfer::export_moving(
        &paths,
        space,
        unsaved.clone(),
        destination.clone(),
        carrying.clone(),
        false,
    );
    packing.await.unwrap();
    let phase = match destination {
        MoveDestination::Ssh(_) | MoveDestination::Client { .. } => MovePhase::Delivering,
        _ => MovePhase::Delivered,
    };
    let record = MoveRecord {
        space: workspace.id.clone(),
        destination,
        carrying,
        unsaved,
        phase,
        checkpoints: Default::default(),
        saved_before: Default::default(),
    };
    (paths, record, workspace.path.clone())
}

#[tokio::test]
async fn a_move_whose_file_has_its_package_ends_the_space_here() {
    isolated("a_move_whose_file_has_its_package_ends_the_space_here_child").await;
}

#[tokio::test]
#[ignore = "isolated helper of a_move_whose_file_has_its_package_ends_the_space_here"]
async fn a_move_whose_file_has_its_package_ends_the_space_here_child() {
    if !helper() {
        return;
    }
    let daemon = TestDaemon::start().await;
    let file = |home: &std::path::Path| MoveDestination::File {
        path: home.join("moving.shoal"),
    };
    let (paths, record, path) = left_move(&daemon, file).await;

    let report = finish(&paths, record, &Unfollowed).await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert!(report.moved[0].manifest.is_some());
    assert!(!path.exists());
}

#[tokio::test]
async fn a_move_that_stopped_while_another_machine_imported_it_stays_stopped_here() {
    isolated("a_move_that_stopped_while_another_machine_imported_it_stays_stopped_here_child")
        .await;
}

#[tokio::test]
#[ignore = "isolated helper of a_move_that_stopped_while_another_machine_imported_it_stays_stopped_here"]
async fn a_move_that_stopped_while_another_machine_imported_it_stays_stopped_here_child() {
    if !helper() {
        return;
    }
    let daemon = TestDaemon::start().await;
    let ssh = |_: &std::path::Path| {
        MoveDestination::Ssh(SshDestination {
            host: "mini".into(),
            shoal: None,
            resume: true,
        })
    };
    let (paths, record, path) = left_move(&daemon, ssh).await;
    let id = record.space.clone();

    let report = finish(&paths, record.clone(), &Unfollowed).await.unwrap();
    assert!(report.failed[0].error.contains("mini may already have it"));
    assert!(path.exists());
    // The move ended, so nothing finishes it again.
    let left = crate::client::transfer::abandon_move(&paths, SpaceSelector::id(&id));
    assert!(left.await.unwrap().is_empty());
}

#[tokio::test]
async fn a_move_whose_worker_still_runs_is_left_to_it() {
    isolated("a_move_whose_worker_still_runs_is_left_to_it_child").await;
}

#[tokio::test]
#[ignore = "isolated helper of a_move_whose_worker_still_runs_is_left_to_it"]
async fn a_move_whose_worker_still_runs_is_left_to_it_child() {
    if !helper() {
        return;
    }
    let daemon = TestDaemon::start().await;
    let file = |home: &std::path::Path| MoveDestination::File {
        path: home.join("moving.shoal"),
    };
    let (paths, record, path) = left_move(&daemon, file).await;
    let held = MoveLock::try_take(&paths, &record.space).unwrap().unwrap();

    let report = finish(&paths, record, &Unfollowed).await.unwrap();
    assert!(report.moved.is_empty() && report.failed.is_empty());
    assert!(path.exists());
    held.release();
}

#[tokio::test]
async fn a_package_its_command_took_before_it_went_leaves_the_space_stopped_here() {
    isolated("a_package_its_command_took_before_it_went_leaves_the_space_stopped_here_child").await;
}

#[tokio::test]
#[ignore = "isolated helper of a_package_its_command_took_before_it_went_leaves_the_space_stopped_here"]
async fn a_package_its_command_took_before_it_went_leaves_the_space_stopped_here_child() {
    if !helper() {
        return;
    }
    let daemon = TestDaemon::start().await;
    let stream = |home: &std::path::Path| MoveDestination::Client {
        path: home.join("streamed.shoal"),
    };
    let (paths, record, path) = left_move(&daemon, stream).await;

    let report = finish(&paths, record, &Unfollowed).await.unwrap();
    assert!(
        report.failed[0]
            .error
            .contains("what read its package may already have it")
    );
    assert!(path.exists());
}
