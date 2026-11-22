//! How a fetch writes the copy of another machine's file it keeps here, and
//! how the daemon tells a copy nobody changed since from one the user did:
//! the fetch stamps the copy and its directory with one whole second, which
//! an edit in place moves from the file and a save that replaces the file
//! moves from the directory.
use std::{
    fs::File,
    io,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The file in a copy's directory that a fetch writes until the whole copy
/// has come, then renames to the copy.
pub const FETCHING: &str = ".partial";

/// Stamp the copy at `copy`, which a fetch just renamed into place, and its
/// directory as fetched now.
pub fn stamp(copy: &Path) -> io::Result<()> {
    let directory = copy.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let stamp = UNIX_EPOCH + Duration::from_secs(now.as_secs());
    File::open(copy)?.set_modified(stamp)?;
    File::open(directory)?.set_modified(stamp)
}

/// Whether the copy's directory `directory` holds only the copy a fetch
/// finished, both still as [`stamp`] left them.
pub fn is_as_fetched(directory: &Path) -> io::Result<bool> {
    let mut entries = std::fs::read_dir(directory)?;
    let (Some(copy), None) = (entries.next().transpose()?, entries.next()) else {
        return Ok(false);
    };
    if copy.file_name() == FETCHING {
        return Ok(false);
    }
    let stamped = std::fs::symlink_metadata(directory)?.modified()?;
    let whole_second = stamped
        .duration_since(UNIX_EPOCH)
        .is_ok_and(|since| since.subsec_nanos() == 0);
    Ok(whole_second && copy.metadata()?.modified()? == stamped)
}

#[cfg(test)]
mod tests;
