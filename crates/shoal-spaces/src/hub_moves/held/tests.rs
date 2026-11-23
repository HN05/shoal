use crate::hub_moves::held::remove_written_before;
use crate::stand_in::Fixture;
use shoal_core::test_support::repository;
use std::time::{Duration, SystemTime};

use shoal_protocol::space_names::SpaceSelector;

use crate::{removal::UnsavedWork, spaces::MovePacking, transfer::MoveDestination};

#[tokio::test]
async fn a_held_move_waits_for_its_reader_once_packed_until_its_time_is_up() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let path = repository(root.path(), "repo");
    let repo = (manager.register_repository(path.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let work = (manager.create_workspace(&repo.id, "work".into(), None, None, None))
        .await
        .unwrap();
    let packing = MovePacking {
        unsaved: UnsavedWork::All,
        destination: MoveDestination::Held,
        carrying: Default::default(),
        again: false,
    };
    let selector = SpaceSelector::id(&work.id);
    (manager
        .spaces
        .export_moving_space(&*manager, &selector, packing))
    .await
    .unwrap();
    assert!(
        manager
            .spaces
            .held_for_reader()
            .await
            .unwrap()
            .contains(&work.id)
    );

    (manager.store)
        .write(|tx| Ok(tx.execute("UPDATE space_moves SET held_until=0", [])?))
        .await
        .unwrap();

    assert!(
        !manager
            .spaces
            .held_for_reader()
            .await
            .unwrap()
            .contains(&work.id)
    );
}

#[test]
fn only_copies_written_before_their_reader_s_time_ran_out_go() {
    let directory = tempfile::tempdir().unwrap();
    let copy = directory.path().join("copy.shoal");
    std::fs::write(&copy, "package").unwrap();

    remove_written_before(directory.path(), SystemTime::UNIX_EPOCH).unwrap();
    assert!(copy.exists());

    let later = SystemTime::now() + Duration::from_secs(60);
    remove_written_before(directory.path(), later).unwrap();
    assert!(!copy.exists());
}

#[test]
fn no_copies_waiting_leave_nothing_to_remove() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("copies");

    remove_written_before(&missing, SystemTime::now()).unwrap();
}
