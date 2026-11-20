//! A pseudo-terminal the daemon makes for a user on another machine: its
//! controlling side, read and written without blocking and resized as the
//! user's terminal is, and the terminal side a process runs on.
use std::{
    io,
    os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd},
};

use anyhow::{Context as _, Result};
use shoal_protocol::attaching::TerminalSize;
use tokio::io::unix::AsyncFd;

/// The controlling side of a pseudo-terminal. Dropping it hangs up the
/// terminal, which signals the session that runs on it.
pub(super) struct Pty(AsyncFd<OwnedFd>);

/// Open a pseudo-terminal of `size`, returning its controlling side and its
/// terminal side, neither of which a process this one starts inherits but
/// as its standard streams.
pub(super) fn open(size: Option<TerminalSize>) -> Result<(Pty, OwnedFd)> {
    let (mut controller, mut terminal) = (-1, -1);
    let mut size = size.map(winsize);
    let size_pointer = size
        .as_mut()
        .map_or(std::ptr::null_mut(), std::ptr::from_mut);
    // SAFETY: openpty initializes both descriptors on success, and reads
    // `size` only when it is not null.
    let opened = unsafe {
        libc::openpty(
            &mut controller,
            &mut terminal,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            size_pointer,
        )
    };
    if opened != 0 {
        return Err(io::Error::last_os_error()).context("open a terminal");
    }
    // SAFETY: openpty returned both descriptors, which nothing else owns.
    let (controller, terminal) = unsafe {
        (
            OwnedFd::from_raw_fd(controller),
            OwnedFd::from_raw_fd(terminal),
        )
    };
    for fd in [controller.as_raw_fd(), terminal.as_raw_fd()] {
        set_flag(fd, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC)?;
    }
    set_flag(
        controller.as_raw_fd(),
        libc::F_GETFL,
        libc::F_SETFL,
        libc::O_NONBLOCK,
    )?;
    // SAFETY: the `OwnedFd` keeps its descriptor open, naming the same
    // terminal, until the `AsyncFd` drops it.
    let controller = unsafe { AsyncFd::register(controller) };
    let controller = controller.map_err(io::Error::from)?;
    Ok((Pty(controller), terminal))
}

impl Pty {
    /// What the terminal's processes wrote next; 0 once none has it open.
    pub(super) async fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut ready = self.0.readable().await?;
            let read = ready.try_io(|fd| {
                // SAFETY: `buffer` is valid for its length.
                let read =
                    unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
                if read < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(read as usize)
            });
            match read {
                Ok(Ok(read)) => return Ok(read),
                // The terminal side closed, as once its processes exited.
                Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => return Ok(0),
                Ok(Err(error)) => return Err(error),
                Err(_would_block) => {}
            }
        }
    }

    /// Type `bytes` into the terminal.
    pub(super) async fn write_all(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let mut ready = self.0.writable().await?;
            let written = ready.try_io(|fd| {
                // SAFETY: `bytes` is valid for its length.
                let written =
                    unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
                if written < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(written as usize)
            });
            if let Ok(written) = written {
                bytes = &bytes[written?..];
            }
        }
        Ok(())
    }

    /// Give the terminal `size`, which signals its foreground processes.
    pub(super) fn resize(&self, size: TerminalSize) -> io::Result<()> {
        let size = winsize(size);
        // SAFETY: TIOCSWINSZ reads one winsize from `size`.
        let resized = unsafe { libc::ioctl(self.0.as_raw_fd(), libc::TIOCSWINSZ as _, &size) };
        match resized {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

fn winsize(size: TerminalSize) -> libc::winsize {
    libc::winsize {
        ws_row: size.rows,
        ws_col: size.columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

/// Add `flag` to the flags of `fd` that `get` reads and `set` writes.
fn set_flag(fd: RawFd, get: libc::c_int, set: libc::c_int, flag: libc::c_int) -> Result<()> {
    // SAFETY: fcntl reads and writes the flags of a descriptor this
    // process owns.
    let changed = unsafe {
        let flags = libc::fcntl(fd, get);
        flags >= 0 && libc::fcntl(fd, set, flags | flag) == 0
    };
    match changed {
        true => Ok(()),
        false => Err(io::Error::last_os_error()).context("set a terminal's flags"),
    }
}
