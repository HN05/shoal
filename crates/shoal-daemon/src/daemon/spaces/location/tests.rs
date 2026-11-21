use shoal_protocol::space_names::SpaceSelector;

use crate::daemon::{scope::Caller, test_daemon::TestDaemon};
use crate::refusal::Reason;
use shoal_protocol::model::Located;

#[tokio::test]
async fn a_space_here_is_located_here_and_one_outside_the_caller_s_scope_is_not() {
    let daemon = TestDaemon::start().await;
    let manager = &daemon.manager;
    let (own, other) = (
        daemon.workspace("own").await,
        daemon.workspace("other").await,
    );
    let selector = SpaceSelector::id(&other.id);
    let located = manager.locate_space(None, &selector).await.unwrap();
    assert!(matches!(&located, Located::Here(space) if space.id == other.id));
    // A sibling is outside the scope of the other space's process.
    let caller = Caller::of_space(own.id.clone());
    let refused = manager.locate_space(Some(&caller), &selector).await;
    assert_eq!(Reason::of(&refused.unwrap_err()), Some(Reason::NotFound));
}

#[tokio::test]
async fn a_space_no_machine_has_is_refused_as_not_found() {
    let daemon = TestDaemon::start().await;
    let unknown = SpaceSelector::Number(99.into());
    let refused = daemon.manager.locate_space(None, &unknown).await;
    assert_eq!(Reason::of(&refused.unwrap_err()), Some(Reason::NotFound));
}
