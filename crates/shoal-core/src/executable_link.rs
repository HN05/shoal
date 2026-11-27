//! The link in the state directory that the daemon points at the binary it
//! started through. Commands an agent's environment keeps for its whole run
//! start Shoal through it, so they reach Shoal after its install moves.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// The link of the state directory `state`.
pub fn path(state: &Path) -> PathBuf {
    state.join("bin").join("shoal")
}

/// Point the link of `state` at the binary this process started through:
/// the path, such as an installation symlink, rather than the file there,
/// so it follows an upgrade in place too.
pub fn point(state: &Path) -> io::Result<()> {
    let target = match crate::installed::started().path() {
        Some(path) => path.to_owned(),
        None => crate::process::executable::invoked_executable()?,
    };
    aim(&path(state), &target)
}

/// Point `link` at `target`, unless `target` is the link itself, as for a
/// process started through it: the link then keeps the installation it
/// names instead of naming itself.
fn aim(link: &Path, target: &Path) -> io::Result<()> {
    if names(target, link) {
        return Ok(());
    }
    replace(link, target)
}

/// Whether `path` is `link`, through whatever symlinks its directories take.
fn names(path: &Path, link: &Path) -> bool {
    let resolved = |path: &Path| {
        let directory = fs::canonicalize(path.parent()?).ok()?;
        Some(directory.join(path.file_name()?))
    };
    path == link || resolved(path).is_some_and(|path| Some(path) == resolved(link))
}

/// Replace the link at `link` with one to `target` in one rename, so a
/// command never finds it missing.
fn replace(link: &Path, target: &Path) -> io::Result<()> {
    let directory = link.parent().expect("the link is inside a directory");
    fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(".shoal-{}", std::process::id()));
    match fs::remove_file(&temporary) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    std::os::unix::fs::symlink(target, &temporary)?;
    fs::rename(&temporary, link)
}

#[cfg(test)]
mod tests;
