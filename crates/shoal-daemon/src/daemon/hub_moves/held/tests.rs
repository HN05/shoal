use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{
        spaces::MovePacking,
        test_support::{manager, repository},
    },
    removal::UnsavedWork,
    transfer::MoveDestination,
};

#[tokio::test]
async fn a_held_move_waits_for_its_reader_once_packed_until_its_time_is_up() {
    let (root, manager) = manager().await;
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
    (manager.export_moving_space(&selector, packing))
        .await
        .unwrap();
    assert!(manager.held_for_reader().await.unwrap().contains(&work.id));

    (manager.store)
        .write(|tx| Ok(tx.execute("UPDATE space_moves SET held_until=0", [])?))
        .await
        .unwrap();

    assert!(!manager.held_for_reader().await.unwrap().contains(&work.id));
}
