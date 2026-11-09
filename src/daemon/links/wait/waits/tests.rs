use super::*;

#[tokio::test]
async fn a_newer_wait_supersedes_the_older_one_per_workspace() {
    let waits = ActiveWaits::default();
    let older = waits.start(&SpaceId::from("one"));
    let other = waits.start(&SpaceId::from("two"));
    let newer = waits.start(&SpaceId::from("one"));
    older.superseded.notified().await;
    assert!(!older.is_current());
    assert!(newer.is_current() && other.is_current());
    drop(older);
    assert!(newer.is_current());
    drop(newer);
    assert!(!waits.is_waiting(&SpaceId::from("one")));
}
