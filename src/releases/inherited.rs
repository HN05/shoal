//! What a wrapper hands the program it replaces itself with through the
//! environment: the path it was started through, with the identity of the
//! file there, and the descriptors of the release locks it holds, which stay
//! open across the replacement.
use std::{
    ffi::OsString,
    io,
    os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd},
    path::PathBuf,
    sync::OnceLock,
};

use crate::{
    env,
    fsutil::{FileIdentity, set_inherited},
};

/// The path a process was started through, and the file there at the time.
#[derive(Debug, Clone)]
pub(crate) struct Invoked {
    pub(crate) path: PathBuf,
    pub(crate) identity: Option<FileIdentity>,
}

impl Invoked {
    /// `path`, with the file there now.
    pub(crate) fn at(path: PathBuf) -> Self {
        let identity = FileIdentity::of(&path).ok();
        Self { path, identity }
    }
}

struct Inherited {
    invoked: Option<Invoked>,
    locks: Vec<OwnedFd>,
}

static INHERITED: OnceLock<Inherited> = OnceLock::new();

/// Keep what the process this one replaced handed it, close its locks to the
/// programs this one starts, and drop the variables so they never reach them.
///
/// # Safety
/// Call before any other thread exists.
pub(crate) unsafe fn take() {
    let invoked = std::env::var_os(env::RELEASE_INVOKED).map(|path| Invoked {
        path: path.into(),
        identity: std::env::var(env::RELEASE_INVOKED_IDENTITY)
            .ok()
            .and_then(|identity| serde_json::from_str(&identity).ok()),
    });
    let locks = std::env::var(env::RELEASE_LOCKS)
        .unwrap_or_default()
        .split(':')
        .filter_map(|fd| fd.parse().ok())
        .filter_map(adopt)
        .collect();
    for name in [
        env::RELEASE_INVOKED,
        env::RELEASE_INVOKED_IDENTITY,
        env::RELEASE_LOCKS,
    ] {
        // SAFETY: the caller guarantees no other thread reads the environment.
        unsafe { std::env::remove_var(name) };
    }
    let _ = INHERITED.set(Inherited { invoked, locks });
}

/// The handed descriptor `fd`, closed to the programs this process starts;
/// none when it is not open.
fn adopt(fd: RawFd) -> Option<OwnedFd> {
    // SAFETY: the process this one replaced handed it the open descriptor,
    // which nothing else in this process owns.
    (fd > 2 && set_inherited(fd, false).is_ok()).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The path the process this one replaced was started through.
pub(crate) fn invoked() -> Option<&'static Invoked> {
    INHERITED.get()?.invoked.as_ref()
}

/// The descriptors of the release locks this process was handed.
pub(crate) fn locks() -> impl Iterator<Item = RawFd> {
    INHERITED
        .get()
        .into_iter()
        .flat_map(|inherited| inherited.locks.iter().map(OwnedFd::as_raw_fd))
}

/// The variables that hand `invoked` and `locks` to the program this process
/// replaces itself with, marking the locks to stay open across the
/// replacement.
pub(crate) fn handover(
    invoked: &Invoked,
    locks: &[RawFd],
) -> io::Result<Vec<(&'static str, OsString)>> {
    for &fd in locks {
        set_inherited(fd, true).map_err(io::Error::other)?;
    }
    let mut variables = vec![(env::RELEASE_INVOKED, invoked.path.clone().into_os_string())];
    if let Some(identity) = invoked.identity {
        variables.push((
            env::RELEASE_INVOKED_IDENTITY,
            serde_json::to_string(&identity)?.into(),
        ));
    }
    let locks: Vec<_> = locks.iter().map(ToString::to_string).collect();
    variables.push((env::RELEASE_LOCKS, locks.join(":").into()));
    Ok(variables)
}
