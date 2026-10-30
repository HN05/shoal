//! Where the daemon listens. Shoal's own copy is `src/paths.rs`; a test
//! there checks that both agree.
use std::{
    io,
    path::{Path, PathBuf},
};

/// The variable that names Shoal's state directory.
pub const STATE_DIR_VARIABLE: &str = "SHOAL_STATE_DIR";

/// Shoal's state directory below the home directory, unless
/// [`STATE_DIR_VARIABLE`] names another.
const DEFAULT_STATE: &str = ".local/state/shoal";

/// The socket the current user's daemon listens on: `daemon.sock` in
/// `SHOAL_STATE_DIR`, or in `~/.local/state/shoal`.
pub fn default_socket() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    let state = std::env::var_os(STATE_DIR_VARIABLE).map(PathBuf::from);
    socket_in(state.as_deref(), &home)
}

/// The socket in the state directory `state`, or the default one below
/// `home`. A relative directory is taken from the current directory.
pub fn socket_in(state: Option<&Path>, home: &Path) -> io::Result<PathBuf> {
    if !home.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HOME must be an absolute path",
        ));
    }
    let state = state.map_or_else(|| home.join(DEFAULT_STATE), Path::to_path_buf);
    let state = if state.is_absolute() {
        state
    } else {
        std::env::current_dir()?.join(state)
    };
    Ok(state.join("daemon.sock"))
}
