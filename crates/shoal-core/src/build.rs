//! The kind of build that is running. Debug builds are development builds: they
//! share the user's home with the installed Shoal, so they leave its user-level
//! files, those outside the state directory, to it.

/// Whether this build may change user-level files. Release builds may; debug
/// builds only when told to, as tests in a temporary home do.
pub fn changes_user_files() -> bool {
    !cfg!(debug_assertions)
        || std::env::var_os(shoal_protocol::env::DEV_USER_FILES).is_some_and(|value| value == "1")
}

/// Whether this build may change the files under `home`: the user's own
/// where [`changes_user_files`], and another home's, such as a test's
/// temporary one, always.
pub fn changes_files_under(home: &std::path::Path) -> bool {
    changes_user_files()
        || crate::fsutil::home::account_home()
            .is_some_and(|user| !crate::paths::same_directory(home, &user))
}
