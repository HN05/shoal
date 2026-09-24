use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
};

/// A nonblocking master for transcript collection and a slave for test-selected
/// stdio streams. Keep a slave handle alive while draining output on macOS.
pub fn open() -> (File, File) {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: openpty initializes both descriptors; File owns them below.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    assert_ne!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        -1
    );
    (master, slave)
}
