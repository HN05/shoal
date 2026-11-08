//! Free disk space.
use std::{io, path::Path};

/// Space unprivileged processes may still write on the filesystem holding `path`.
pub fn available_bytes(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: the path is NUL terminated and statvfs fills the zeroed buffer.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs succeeded, so the buffer is initialized.
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::useless_conversion)] // The field types differ by platform.
    Ok(u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize)))
}

#[cfg(test)]
mod tests;
