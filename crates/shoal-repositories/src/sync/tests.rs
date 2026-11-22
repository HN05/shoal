use futures_util::FutureExt as _;
use shoal_ids::repository_ids::RepositoryId;

use super::Syncs;

#[test]
fn a_request_wakes_the_loop_for_that_repository() {
    let syncs = Syncs::default();
    let id = RepositoryId::from("repo".to_owned());
    syncs.request(&id);
    assert!(syncs.wake.notified().now_or_never().is_some());
    assert_eq!(syncs.take_requested(), vec![id]);
    assert!(syncs.take_requested().is_empty());
}
