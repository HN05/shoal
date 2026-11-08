//! Executables: finding them on a search path and naming the one this
//! process runs.
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

/// The absolute path this process was started through. Unlike `current_exe`
/// on Linux, it keeps an installation symlink instead of resolving into a
/// versioned directory, so a later run of it reaches an upgraded binary.
/// Once that path names another binary, it is the file this process runs.
/// A process started from a release copy was started through the path the
/// process it replaced handed it, while the file there is the one it was then.
pub fn invoked_executable() -> io::Result<PathBuf> {
    let current = running_executable()?;
    if let Some(invoked) = crate::releases::invoked() {
        return Ok(if invoked.unchanged() {
            invoked.path.clone()
        } else {
            current
        });
    }
    Ok(invoked_path(
        std::env::args_os().next(),
        &std::env::var_os("PATH").unwrap_or_default(),
        &std::env::current_dir()?,
        current,
    ))
}

/// The file this process runs. macOS's `current_exe` is the path as
/// started, so it would follow a replaced symlink too; the kernel's path for
/// the image does not.
pub fn running_executable() -> io::Result<PathBuf> {
    match crate::process::identity::executable(std::process::id()) {
        Some(path) => Ok(path),
        None => std::env::current_exe(),
    }
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

// Trust argv[0] only while it names the running binary.
fn invoked_path(
    arg: Option<std::ffi::OsString>,
    search_path: &OsStr,
    cwd: &Path,
    current: PathBuf,
) -> PathBuf {
    // A relative argv[0] or PATH entry names a file in the current directory.
    let invoked = arg.map(PathBuf::from).and_then(|arg| {
        if arg.components().count() > 1 {
            Some(cwd.join(arg))
        } else {
            std::env::split_paths(search_path)
                .map(|directory| cwd.join(directory).join(&arg))
                .find(|path| is_executable(path).unwrap_or(false))
        }
    });
    match invoked {
        Some(path) if same_file(&path, &current) => path,
        _ => current,
    }
}

fn same_file(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (fs::metadata(left), fs::metadata(right)) {
        (Ok(left), Ok(right)) => (left.dev(), left.ino()) == (right.dev(), right.ino()),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
