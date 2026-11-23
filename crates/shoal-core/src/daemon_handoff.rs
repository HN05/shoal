//! Keep ownership and queued connections across a managed daemon update,
//! and reap the children it leaves the new image.
use crate::paths::StateFiles;
use crate::{fsutil::descriptors::set_inherited, paths::Paths, process::identity};
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
pub struct Handoff {
    pub lock: File,
    pub listener: StdListener,
    /// The children the replaced image left this process, such as detached
    /// executions' wrappers, whose waits ended with that image.
    pub inherited_children: Vec<u32>,
}

impl Handoff {
    /// Main consumes the private handoff before creating threads or descriptor
    /// owners. Children must not inherit the new daemon's lock or socket.
    pub fn take() -> Result<Option<Self>> {
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
        // Every child is inherited until this image starts its own, which it
        // waits on itself.
        let inherited_children = identity::children().unwrap_or_else(|error| {
            eprintln!("warning: could not list the inherited children to reap: {error:#}");
            Vec::new()
        });
        // SAFETY: these distinct open descriptors transferred by exec have no
        // other Rust owners before the new process creates its runtime.
        Ok(Some(unsafe {
            Self {
                lock: File::from_raw_fd(lock),
                listener: StdListener::from_raw_fd(listener),
                inherited_children,
            }
        }))
    }

    /// Reap each inherited child once it exits, which nothing else waits
    /// for; each waits on a thread of its own, since a blocking task would
    /// hold up the runtime's shutdown until the child exits.
    pub fn reap_inherited_children(&mut self) {
        for pid in std::mem::take(&mut self.inherited_children) {
            let spawned = std::thread::Builder::new()
                .name("reap inherited".to_owned())
                .spawn(move || reap(pid));
            if let Err(error) = spawned {
                eprintln!("warning: could not reap inherited child {pid}: {error}");
            }
        }
    }

    pub fn verify(&self, paths: &Paths) -> Result<()> {
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
pub fn exec(path: &Path, paths: &Paths, listener: &UnixListener, lock: &File) -> Result<()> {
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

/// Wait for the child `pid` to exit and reap it.
fn reap(pid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    loop {
        let mut status = 0;
        // SAFETY: waitpid writes only the status, which outlives the call.
        if unsafe { libc::waitpid(pid, &mut status, 0) } == pid {
            return;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            eprintln!("warning: could not reap inherited child {pid}: {error}");
            return;
        }
    }
}

#[cfg(test)]
mod tests;
