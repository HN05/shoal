//! Executables: finding them on a search path and the app bundle one is
//! the main program of.
use std::{
    ffi::OsStr,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Follow symlinks and require a regular file with at least one execute bit.
pub fn is_executable(path: &Path) -> io::Result<bool> {
    let metadata = fs::metadata(path)?;
    Ok(metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Search a supplied PATH in order, ignoring inaccessible candidates.
pub fn find_executable(program: &OsStr, search_path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .map(|directory| directory.join(program))
        .find(|path| is_executable(path).unwrap_or(false))
}

/// The app bundle `executable` is the main program of, as in
/// `Shoal.app/Contents/MacOS/shoal`.
pub fn app_bundle(executable: &Path) -> Option<&Path> {
    let directory = executable.parent()?;
    let bundle = directory.parent()?.parent()?;
    (directory.ends_with("Contents/MacOS")
        && bundle
            .extension()
            .is_some_and(|extension| extension == "app"))
    .then_some(bundle)
}

#[cfg(test)]
mod tests;
