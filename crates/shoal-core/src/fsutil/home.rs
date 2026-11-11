//! The user's home directory.
use std::{ffi::OsStr, path::PathBuf};

pub use crate::client::fsutil::expand_home;
pub use crate::client::paths::home_dir;

/// The home directory of the user this process runs as, from the password
/// database rather than `HOME`.
pub(crate) fn account_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut record: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 16 * 1024];
    let result = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            &mut record,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        )
    };
    if result != 0 || found.is_null() || record.pw_dir.is_null() {
        return None;
    }
    let home = unsafe { std::ffi::CStr::from_ptr(record.pw_dir) };
    Some(PathBuf::from(OsStr::from_bytes(home.to_bytes())))
}

#[cfg(test)]
mod tests;
