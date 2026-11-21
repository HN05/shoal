use super::*;
use crate::moves::fixtures::{Unfollowed, helper, left_move};
use shoal_core::transfer::SshDestination;
use shoal_daemon::test_daemon::TestDaemon;

/// Run this module's helper `name` in a process of its own.
async fn isolated(name: &str) {
    crate::moves::fixtures::isolated(&format!("moves::finishing::tests::{name}")).await;
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

#[tokio::test]
async fn a_held_move_given_back_keeps_the_space_here_and_drops_its_package() {
    isolated("a_held_move_given_back_keeps_the_space_here_and_drops_its_package_child").await;
}

#[tokio::test]
#[ignore = "isolated helper of a_held_move_given_back_keeps_the_space_here_and_drops_its_package"]
async fn a_held_move_given_back_keeps_the_space_here_and_drops_its_package_child() {
    if !helper() {
        return;
    }
    let daemon = TestDaemon::start().await;
    let (paths, mut record, path) = left_move(&daemon, |_| MoveDestination::Held).await;
    let package = (daemon.manager.paths.move_packages())
        .join(shoal_core::transfer::package_name(&record.space));
    assert!(package.exists());
    // As the reader's give-back records it.
    record.phase = MovePhase::Packing;

    let report = finish(&paths, record, &Unfollowed).await.unwrap();

    assert!(
        report.failed[0].error.contains("it stays here"),
        "{report:?}"
    );
    assert!(path.exists());
    assert!(!package.exists());
}
