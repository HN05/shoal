//! Another process's terminal, and how it takes typed input.
use std::{
    fs::OpenOptions,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

/// The terminal `pid` runs on, or `None` for a process without one or whose
/// records cannot be read.
pub(crate) fn of(pid: u32) -> Option<PathBuf> {
    super::identity::details::terminal(pid).ok().flatten()
}

/// Whether `tty` takes input a line at a time, as a terminal does until a
/// line editor reads it key by key; `None` when it cannot be read.
pub(crate) fn canonical(tty: &Path) -> Option<bool> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(tty)
        .ok()?;
    let mut settings = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: tcgetattr initializes settings on success.
    if unsafe { libc::tcgetattr(file.as_raw_fd(), settings.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: tcgetattr succeeded.
    let settings = unsafe { settings.assume_init() };
    Some(settings.c_lflag & libc::ICANON != 0)
}

#[cfg(test)]
mod tests;
