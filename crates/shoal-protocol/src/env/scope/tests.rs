use super::*;

#[test]
fn a_granted_scope_token_stands_in_until_it_is_released() {
    act_in_space_scope(ScopeToken::from("own"));
    act_under_grant(ScopeToken::from("granted"));
    assert_eq!(scope_token(), Some(ScopeToken::from("granted")));
    release_grant();
    assert_eq!(scope_token(), Some(ScopeToken::from("own")));
}

#[test]
fn acting_in_a_space_scope_drops_a_grant() {
    act_under_grant(ScopeToken::from("granted"));
    act_in_space_scope(ScopeToken::from("space"));
    assert_eq!(scope_token(), Some(ScopeToken::from("space")));
}
