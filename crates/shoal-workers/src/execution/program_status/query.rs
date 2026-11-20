//! Asking a terminal whether it reads the Program Status Protocol. The
//! protocol's query goes out followed by a primary device attributes
//! request, which every terminal answers: a terminal that reads the
//! protocol replies to its query first, so a device attributes reply
//! without one before it means the terminal does not.
use std::{
    fs::File,
    io::{Read as _, Write as _},
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

const QUERY: &[u8] = b"\x1b]7501;?\x1b\\\x1b[c";
/// How a terminal's reply to the protocol's query starts.
const PROTOCOL_REPLY: &[u8] = b"\x1b]7501;?";
/// How a primary device attributes reply starts, before its parameters and
/// final `c`.
const ATTRIBUTES_REPLY: &[u8] = b"\x1b[?";

/// Whether `terminal` reads the protocol, by its replies within `limit`,
/// which guards only against a terminal that answers neither request.
/// Input typed meanwhile is read with the replies and lost.
pub(super) fn reads_protocol(terminal: &File, limit: Duration) -> bool {
    let Some(_quiet) = Quiet::enter(terminal) else {
        return false;
    };
    if (&*terminal).write_all(QUERY).is_err() {
        return false;
    }
    let deadline = Instant::now() + limit;
    let mut replies = Vec::new();
    loop {
        if let Some(reads) = answer(&replies) {
            return reads;
        }
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        if !readable(terminal, left) {
            break;
        }
        let mut read = [0; 256];
        match (&*terminal).read(&mut read) {
            Ok(0) => break,
            Ok(count) => replies.extend_from_slice(&read[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    contains(&replies, PROTOCOL_REPLY)
}

/// What `replies` say once the device attributes reply ends: whether the
/// protocol's reply came before it.
fn answer(replies: &[u8]) -> Option<bool> {
    attributes_end(replies).map(|end| contains(&replies[..end], PROTOCOL_REPLY))
}

/// Where the first complete device attributes reply in `replies` ends.
fn attributes_end(replies: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(start) = find(&replies[from..], ATTRIBUTES_REPLY) {
        let parameters = from + start + ATTRIBUTES_REPLY.len();
        let rest = &replies[parameters..];
        let length = (rest.iter())
            .position(|byte| !byte.is_ascii_digit() && *byte != b';')
            .unwrap_or(rest.len());
        match rest.get(length) {
            Some(b'c') => return Some(parameters + length + 1),
            None => return None,
            Some(_) => from = parameters,
        }
    }
    None
}

fn find(bytes: &[u8], wanted: &[u8]) -> Option<usize> {
    bytes
        .windows(wanted.len())
        .position(|window| window == wanted)
}

fn contains(bytes: &[u8], wanted: &[u8]) -> bool {
    find(bytes, wanted).is_some()
}

/// Whether `terminal` has input within `limit`.
fn readable(terminal: &File, limit: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: terminal.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = limit.as_millis().clamp(1, i32::MAX as u128) as i32;
    // SAFETY: polls one descriptor this process holds open.
    unsafe { libc::poll(&mut poll, 1, millis) > 0 }
}

/// The terminal with echo and line editing off, so its replies are read as
/// they arrive and never shown; restored when dropped.
struct Quiet<'a> {
    terminal: &'a File,
    previous: libc::termios,
}

impl<'a> Quiet<'a> {
    fn enter(terminal: &'a File) -> Option<Self> {
        let fd = terminal.as_raw_fd();
        let mut settings = std::mem::MaybeUninit::uninit();
        // SAFETY: tcgetattr initializes settings on success.
        if unsafe { libc::tcgetattr(fd, settings.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: initialized by the successful call above.
        let previous = unsafe { settings.assume_init() };
        let mut quiet = previous;
        quiet.c_lflag &= !(libc::ICANON | libc::ECHO);
        quiet.c_cc[libc::VMIN] = 0;
        quiet.c_cc[libc::VTIME] = 0;
        // SAFETY: applies settings derived from the terminal's own.
        (unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) } == 0)
            .then_some(Self { terminal, previous })
    }
}

impl Drop for Quiet<'_> {
    fn drop(&mut self) {
        // SAFETY: restores the settings read when the terminal went quiet.
        unsafe {
            libc::tcsetattr(self.terminal.as_raw_fd(), libc::TCSANOW, &self.previous);
        }
    }
}
