//! File descriptors this process hands to the programs it execs.
use std::io;

/// Set whether descriptor `fd` stays open in a program this process execs.
pub fn set_inherited(fd: std::os::fd::RawFd, inherited: bool) -> anyhow::Result<()> {
    // SAFETY: fcntl validates the descriptor; neither operation uses a pointer.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    anyhow::ensure!(
        flags >= 0,
        "inspect descriptor {fd}: {}",
        io::Error::last_os_error()
    );
    let flags = if inherited {
        flags & !libc::FD_CLOEXEC
    } else {
        flags | libc::FD_CLOEXEC
    };
    anyhow::ensure!(
        // SAFETY: as above.
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } == 0,
        "configure descriptor {fd}: {}",
        io::Error::last_os_error()
    );
    Ok(())
}
