use shoal_protocol::launches::LaunchSource;

/// A restore the user runs here is theirs; nextest runs each test in a
/// process of its own, so no queue entry was taken here.
#[test]
fn a_restore_without_a_queue_entry_is_the_users() {
    assert_eq!(super::restore_source(), Some(LaunchSource::Manual));
}

/// A restore the queue started for an admitted entry is not the user's:
/// its queuing decided whether it counts.
#[test]
fn a_restore_the_queue_started_is_not_the_users() {
    crate::env::launch_queue_entry(&"entry".into());

    assert_eq!(super::restore_source(), None);
}
