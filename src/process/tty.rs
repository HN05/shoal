//! Another process's terminal, and how it takes typed input.
use std::{
    fs::OpenOptions,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

use crate::tools::Tool;

/// The terminal `pid` runs on, or `None` for a process without one or that
/// `ps` cannot read.
pub async fn of(pid: u32) -> Option<PathBuf> {
    let mut command = tokio::process::Command::new(Tool::Ps.program());
    command
        .args(["-o", "tty=", "-p", &pid.to_string()])
        .env("LC_ALL", "C");
    let output = crate::subprocess::Run::new(command).checked().await.ok()?;
    device(&String::from_utf8_lossy(&output.stdout))
}

/// The device of the terminal `ps` names `name`, as `ttys004` or `pts/3`;
/// it shows a process without one as `?` or `??`.
fn device(name: &str) -> Option<PathBuf> {
    let name = name.trim();
    (!name.is_empty() && !name.starts_with('?')).then(|| Path::new("/dev").join(name))
}

/// Whether `tty` takes input a line at a time, as a terminal does until a
/// line editor reads it key by key; `None` when it cannot be read.
pub fn canonical(tty: &Path) -> Option<bool> {
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
