use super::*;

#[test]
fn a_development_build_refuses_only_the_users_own_state_directory() {
    let Some(home) = crate::fsutil::account_home() else {
        return;
    };
    let user = home.join(DEFAULT_STATE);
    let refused = refuse_user_state(&user);
    assert_eq!(
        refused.is_ok(),
        crate::build::changes_user_files(),
        "{refused:?}"
    );
    let temporary = tempfile::tempdir().unwrap();
    refuse_user_state(&temporary.path().join(DEFAULT_STATE)).unwrap();
}
