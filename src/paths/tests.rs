use super::*;

#[test]
fn a_development_build_refuses_only_the_users_own_state_directory() {
    let Some(home) = crate::fsutil::account_home() else {
        return;
    };
    let user = home.join(DEFAULT_STATE);
    let refused = refuse_user_state(&user).map_err(|error| error.to_string());
    if crate::build::changes_user_files() {
        assert_eq!(refused, Ok(()));
    } else {
        assert_eq!(
            refused,
            Err(format!(
                "a development build does not use the user's state directory {}; pass --state-dir with a temporary directory, or set SHOAL_DEV_USER_FILES=1 to use it",
                user.display()
            ))
        );
    }
    let temporary = tempfile::tempdir().unwrap();
    refuse_user_state(&temporary.path().join(DEFAULT_STATE)).unwrap();
}
