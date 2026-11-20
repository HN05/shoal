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

#[test]
fn a_lock_file_its_holder_deleted_no_longer_names_the_lock() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let id = SpaceId::from("moving");
    let held = MoveLock::try_take(&paths, &id).unwrap().unwrap();
    // As a process waiting for the lock opened the file before its release.
    let path = lock_path(&paths, &id).unwrap();
    let waiting = open(&path).unwrap();

    held.release();
    assert!(!still_named(&path, &waiting));
    let taken = MoveLock::try_take(&paths, &id).unwrap().unwrap();
    assert!(still_named(&path, &taken._file));
}
