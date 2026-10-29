//! Keep ownership and queued connections across a managed daemon update.
use crate::{fsutil::set_inherited, paths::Paths};
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{fs::MetadataExt, net::UnixListener as StdListener, process::CommandExt},
    },
    path::Path,
    process::Command,
};
use tokio::net::UnixListener;

use crate::env::DAEMON_HANDOFF;

#[derive(Debug)]
pub(crate) struct Handoff {
    pub lock: File,
    pub listener: StdListener,
}

impl Handoff {
    /// Main consumes the private handoff before creating threads or descriptor
    /// owners. Children must not inherit the new daemon's lock or socket.
    pub(crate) fn take() -> Result<Option<Self>> {
        let Some(value) = std::env::var_os(DAEMON_HANDOFF) else {
            return Ok(None);
        };
        // SAFETY: main calls this before starting the runtime.
        unsafe { std::env::remove_var(DAEMON_HANDOFF) };
        let value = value.to_str().context("invalid daemon handoff")?;
        let (lock, listener) = value.split_once(',').context("invalid daemon handoff")?;
        let lock: RawFd = lock.parse()?;
        let listener: RawFd = listener.parse()?;
        ensure!(
            lock > 2 && listener > 2 && lock != listener,
            "invalid daemon handoff descriptors"
        );
        set_inherited(lock, false)?;
        set_inherited(listener, false)?;
        // SAFETY: these distinct open descriptors transferred by exec have no
        // other Rust owners before the new process creates its runtime.
        Ok(Some(unsafe {
            Self {
                lock: File::from_raw_fd(lock),
                listener: StdListener::from_raw_fd(listener),
            }
        }))
    }

    pub(super) fn verify(&self, paths: &Paths) -> Result<()> {
        let expected = fs::metadata(paths.daemon_lock())?;
        let actual = self.lock.metadata()?;
        ensure!(
            expected.dev() == actual.dev() && expected.ino() == actual.ino(),
            "daemon handoff lock belongs to another state directory"
        );
        ensure!(
            self.listener.local_addr()?.as_pathname() == Some(paths.socket.as_path()),
            "daemon handoff socket belongs to another state directory"
        );
        Ok(())
    }
}

/// Workers and SQLite have drained before descriptors become inheritable.
/// Exec retains the service PID, ownership lock and listening socket backlog.
pub(super) fn exec(path: &Path, paths: &Paths, listener: &UnixListener, lock: &File) -> Result<()> {
    let descriptors = [lock.as_raw_fd(), listener.as_raw_fd()];
    let mut command = Command::new(path);
    command
        .arg("--state-dir")
        .arg(&paths.state)
        .args(["daemon", "run", "--managed"])
        .env(
            DAEMON_HANDOFF,
            format!("{},{}", descriptors[0], descriptors[1]),
        );
    for fd in descriptors {
        set_inherited(fd, true)?;
    }
    let error = command.exec();
    for fd in descriptors {
        set_inherited(fd, false)?;
    }
    Err(error).context("restart updated daemon")
}
