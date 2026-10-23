//! The kind of build that is running. Debug builds are development builds: they
//! share the user's home with the installed Shoal, so they leave its user-level
//! files, those outside the state directory, to it.

/// Whether this build may change user-level files. Release builds may; debug
/// builds only when told to, as tests in a temporary home do.
pub fn changes_user_files() -> bool {
    !cfg!(debug_assertions)
        || std::env::var_os(crate::env::DEV_USER_FILES).is_some_and(|value| value == "1")
}
