use super::*;

#[tokio::test]
async fn a_held_move_lock_is_not_taken_again_until_released() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let id = SpaceId::from("moving");

    let held = MoveLock::take(&paths, &id).await.unwrap();
    assert!(MoveLock::try_take(&paths, &id).unwrap().is_none());
    held.release();
    assert!(MoveLock::try_take(&paths, &id).unwrap().is_some());
}
