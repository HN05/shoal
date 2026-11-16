//! The keys an asleep agent's place reads: its terminal without echo, line
//! editing or signal keys, so typing never garbles the status screen and
//! Ctrl-C reaches the screen as a key.
use std::{
    ffi::CStr,
    fs::File,
    io::{IsTerminal, Read},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
};

use anyhow::{Context, Result};
use tokio::io::unix::AsyncFd;

/// A key that decides the wait.
#[derive(Debug, PartialEq, Eq)]
pub(in super::super) enum Key {
    /// Ctrl-C, or Ctrl-\ as its quit signal did: stop waiting and leave the
    /// agent stopped.
    Stop,
}

/// The terminal on standard input, read in the key mode until dropped,
/// which restores its previous settings.
pub(super) struct Keys {
    terminal: AsyncFd<File>,
    previous: libc::termios,
}

impl Keys {
    /// Read the keys of the terminal on standard input when the screen shows
    /// on standard error and the wrapper holds it in the foreground, as it
    /// does once its agent stopped; `None` otherwise.
    pub(super) fn open() -> Option<Self> {
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal() && foreground()) {
            return None;
        }
        match Self::read_terminal() {
            Ok(keys) => Some(keys),
            Err(error) => {
                eprintln!("shoal: cannot read keys from the terminal: {error:#}");
                None
            }
        }
    }

    fn read_terminal() -> Result<Self> {
        // A file of its own, so its non-blocking reads leave the agent's
        // standard input as it was.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(stdin_terminal()?)
            .context("open the terminal")?;
        let previous = settings(&file)?;
        // SAFETY: the `File` it owns keeps its descriptor open, naming the
        // same terminal, until the `AsyncFd` drops it.
        let terminal = unsafe { AsyncFd::register(file) };
        let terminal = terminal
            .map_err(std::io::Error::from)
            .context("watch the terminal")?;
        apply(terminal.get_ref(), &key_mode(previous))?;
        Ok(Self { terminal, previous })
    }

    /// The next key that decides the wait. A terminal that closed or
    /// failed decides nothing; its session's end stops the wrapper.
    pub(super) async fn next(&mut self) -> Key {
        let mut input = [0; 64];
        loop {
            let Ok(mut ready) = self.terminal.readable_mut().await else {
                return std::future::pending().await;
            };
            match ready.try_io(|terminal| terminal.get_mut().read(&mut input)) {
                Ok(Ok(0)) => return std::future::pending().await,
                Ok(Ok(read)) => {
                    if let Some(key) = key_in(&input[..read]) {
                        return key;
                    }
                }
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Ok(Err(_)) => return std::future::pending().await,
                Err(_would_block) => {}
            }
        }
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        let _ = apply(self.terminal.get_ref(), &self.previous);
    }
}

/// The first key in `input` that decides the wait; others, and the escape
/// sequences of keys such as arrows, decide nothing.
fn key_in(input: &[u8]) -> Option<Key> {
    input.iter().find_map(|byte| match byte {
        0x03 | 0x1c => Some(Key::Stop),
        _ => None,
    })
}

/// `settings` without echo, line editing, signal keys or flow control, so
/// each key reaches the wrapper as it is typed and shows nothing.
fn key_mode(mut settings: libc::termios) -> libc::termios {
    settings.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
    settings.c_iflag &= !libc::IXON;
    settings.c_cc[libc::VMIN] = 1;
    settings.c_cc[libc::VTIME] = 0;
    settings
}

/// Whether this process's group is the foreground of the terminal on
/// standard input, which it reads without being stopped.
fn foreground() -> bool {
    // SAFETY: tcgetpgrp and getpgrp only inspect this process and stdin.
    unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) == libc::getpgrp() }
}

/// The device of the terminal on standard input.
fn stdin_terminal() -> Result<std::path::PathBuf> {
    let mut name = [0 as libc::c_char; 256];
    // SAFETY: ttyname_r writes at most `name.len()` bytes, NUL included.
    let failed = unsafe { libc::ttyname_r(libc::STDIN_FILENO, name.as_mut_ptr(), name.len()) };
    if failed != 0 {
        return Err(std::io::Error::from_raw_os_error(failed)).context("name the terminal");
    }
    // SAFETY: ttyname_r succeeded, so `name` holds a NUL-terminated path.
    let name = unsafe { CStr::from_ptr(name.as_ptr()) };
    Ok(std::ffi::OsStr::from_bytes(name.to_bytes()).into())
}

fn settings(terminal: &File) -> Result<libc::termios> {
    let mut settings = std::mem::MaybeUninit::uninit();
    // SAFETY: tcgetattr initializes settings on success.
    if unsafe { libc::tcgetattr(terminal.as_raw_fd(), settings.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("read the terminal's settings");
    }
    // SAFETY: tcgetattr succeeded.
    Ok(unsafe { settings.assume_init() })
}

fn apply(terminal: &File, settings: &libc::termios) -> Result<()> {
    // SAFETY: `settings` is a complete termios; TCSANOW neither drains output
    // nor discards input.
    if unsafe { libc::tcsetattr(terminal.as_raw_fd(), libc::TCSANOW, settings) } != 0 {
        return Err(std::io::Error::last_os_error()).context("set the terminal's settings");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
