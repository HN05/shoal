//! Directories Shoal creates beside workspaces, marked with their owner.
use std::{fs, io, path::Path};

use super::read_optional;

/// The file naming the owner of a directory Shoal created beside workspaces.
pub const OWNER_MARKER: &str = "owner";

/// Serializes creating marked directories, so no caller in this process finds
/// one another has created but not yet marked. Only the daemon creates them.
static CREATING_MARKED: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Create `root` marked as `owner`'s when it is missing; an existing one,
/// even one appearing meanwhile, is never claimed and is left for
/// [`is_marked_dir`] to judge.
pub fn create_marked_dir(root: &Path, owner: &str) -> io::Result<()> {
    let _creating = CREATING_MARKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::create_dir(root) {
        Ok(()) => fs::write(root.join(OWNER_MARKER), format!("{owner}\n")),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// The owner the real directory `root` is marked as; none for a directory
/// without a readable mark, or anything else.
pub fn marked_owner(root: &Path) -> Option<String> {
    fs::symlink_metadata(root)
        .ok()
        .filter(|metadata| metadata.is_dir())?;
    let marked = read_optional(&root.join(OWNER_MARKER)).ok()??;
    Some(marked.trim_end().to_owned())
}

/// Whether `root` is a real directory marked as `owner`'s, so nothing Shoal
/// creates or deletes there is reached through a symlink or belongs to anyone
/// else.
pub fn is_marked_dir(root: &Path, owner: &str) -> bool {
    check_marked_dir(root, owner).unwrap_or(false)
}

/// [`is_marked_dir`], failing when `root` or its marker exists but cannot be
/// read, rather than taking that for a directory Shoal did not create.
pub fn check_marked_dir(root: &Path, owner: &str) -> io::Result<bool> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    }
    Ok(read_optional(&root.join(OWNER_MARKER))?.is_some_and(|marked| marked.trim_end() == owner))
}

#[cfg(test)]
mod tests;
