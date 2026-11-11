use std::{
    ffi::CStr,
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
};

use super::*;

/// A new terminal's master and slave, which keep its device while open,
/// and the device.
fn terminal() -> (File, File, PathBuf) {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: openpty initializes both descriptors; File owns them below.
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0);
    // SAFETY: openpty returned the descriptors, which nothing else owns.
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    let mut name = [0 as libc::c_char; 128];
    // SAFETY: ttyname_r writes at most the buffer's length.
    let named = unsafe { libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr(), name.len()) };
    assert_eq!(named, 0);
    // SAFETY: ttyname_r wrote a terminated string.
    let name = unsafe { CStr::from_ptr(name.as_ptr()) };
    (master, slave, PathBuf::from(name.to_str().unwrap()))
}

#[test]
fn a_terminal_takes_lines_until_a_line_editor_reads_keys() {
    let (_master, slave, device) = terminal();
    assert_eq!(canonical(&device), Some(true));
    let mut settings = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: tcgetattr initializes settings on success, which it asserts.
    let mut settings = unsafe {
        assert_eq!(libc::tcgetattr(slave.as_raw_fd(), settings.as_mut_ptr()), 0);
        settings.assume_init()
    };
    settings.c_lflag &= !(libc::ICANON | libc::ECHO);
    // SAFETY: settings came from tcgetattr.
    assert_eq!(
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &settings) },
        0
    );
    assert_eq!(canonical(&device), Some(false));
}

#[test]
fn a_missing_terminal_has_no_mode() {
    assert_eq!(canonical(Path::new("/dev/shoal-no-such-tty")), None);
}

#[test]
fn ps_names_a_terminal_by_its_device() {
    assert_eq!(device("ttys004\n"), Some(PathBuf::from("/dev/ttys004")));
    assert_eq!(device(" pts/3"), Some(PathBuf::from("/dev/pts/3")));
    assert_eq!(device("??"), None);
    assert_eq!(device("?"), None);
    assert_eq!(device(""), None);
}
