use shoal_protocol::item_numbers::ItemNumber;

use super::*;
use crate::daemon::test_daemon::TestDaemon;

#[test]
fn a_space_s_process_acts_in_its_own_space_and_the_user_in_the_one_it_names() {
    let named = SpaceSelector::Number(4.into());
    let token = ScopeToken::generate();
    let acting = Target::Acting(&named);
    assert_eq!(target_space(acting, Some(&token)), None);
    assert_eq!(target_space(acting, None), Some(named));
}

#[test]
fn an_item_is_in_the_space_its_number_begins_with() {
    let number = ItemNumber {
        space: 5.into(),
        item: 2,
    };
    let space = target_space(Target::Item(&number), Some(&ScopeToken::generate()));
    assert_eq!(space, Some(SpaceSelector::Number(5.into())));
}

#[tokio::test]
async fn a_token_this_daemon_does_not_know_vouches_for_no_one() {
    let daemon = TestDaemon::start().await;
    let manager = &daemon.manager;
    let workspace = daemon.workspace("here").await;
    let token = daemon.scope_token(&workspace).await;
    let known = scoped_caller(manager, Some(&token)).await.unwrap();
    assert_eq!(
        known.flatten().map(|caller| caller.space_id),
        Some(workspace.id)
    );
    let unknown = ScopeToken::generate();
    assert!(
        scoped_caller(manager, Some(&unknown))
            .await
            .unwrap()
            .is_none()
    );
    // The user's request carries no token and goes as the user's.
    assert!(matches!(
        scoped_caller(manager, None).await.unwrap(),
        Some(None)
    ));
}
