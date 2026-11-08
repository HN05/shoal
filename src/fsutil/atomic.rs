//! Atomic file writes.
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
};

pub enum Permissions {
    /// Keep the private permissions chosen by NamedTempFile.
    Temporary,
    /// Copy the destination's permissions if its metadata can be read.
    Preserve,
    Mode(u32),
}

pub struct ReplaceOptions {
    pub permissions: Permissions,
    /// Sync the replacement file before publishing it; does not sync the directory.
    pub sync: bool,
}

/// Prepare beside the destination so a later persist stays on the same filesystem.
/// The caller creates parent directories and may move a backup before publishing.
pub fn prepare_atomic_write(
    path: &Path,
    contents: &[u8],
    options: ReplaceOptions,
) -> io::Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    let permissions = match options.permissions {
        Permissions::Temporary => None,
        Permissions::Preserve => fs::metadata(path)
            .ok()
            .map(|metadata| metadata.permissions()),
        Permissions::Mode(mode) => Some(fs::Permissions::from_mode(mode)),
    };
    if let Some(permissions) = permissions {
        temporary.as_file().set_permissions(permissions)?;
    }
    if options.sync {
        temporary.as_file().sync_all()?;
    }
    Ok(temporary)
}

/// Replace the directory entry, including an existing symlink, with a complete file.
pub fn replace_atomically(path: &Path, contents: &[u8], options: ReplaceOptions) -> io::Result<()> {
    prepare_atomic_write(path, contents, options)?
        .persist(path)
        .map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests;
