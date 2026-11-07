use super::*;

#[test]
fn a_test_refuses_the_users_own_config_directory_and_the_files_beside_it() {
    let Some(home) = crate::fsutil::account_home() else {
        return;
    };
    let user = home.join(".config/shoal");
    assert!(refuse_user_config(&user.join("config.toml")).is_err());
    assert!(refuse_user_config(&user.join("modes/triage.md")).is_err());
    let temporary = tempfile::tempdir().unwrap();
    refuse_user_config(&temporary.path().join(".config/shoal/config.toml")).unwrap();
}

#[test]
fn refusal_covers_the_directory_through_symlinks_and_paths_not_yet_created() {
    let root = tempfile::tempdir().unwrap();
    let user = root.path().join("user/shoal");
    fs::create_dir_all(&user).unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&user, &link).unwrap();
    let directories = [user.clone()];
    for refused in [
        user.join("config.toml"),
        user.join("modes/triage.md"),
        link.join("config.toml"),
    ] {
        assert!(
            refuse_within(&refused, &directories).is_err(),
            "{}",
            refused.display()
        );
    }
    refuse_within(&root.path().join("user/other/config.toml"), &directories).unwrap();
}
