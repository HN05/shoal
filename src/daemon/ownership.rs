//! Exclusive ownership of a state directory: its lock and listening socket.
use std::{
    fs::{self, File, OpenOptions},
    os::unix::{
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
        net::UnixListener as StdListener,
    },
    path::Path,
};

use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use tokio::net::UnixListener;

use super::handoff::{self, Handoff};
use crate::paths::Paths;

/// Held from startup through shutdown or handoff; dropping it removes the
/// socket. Never unlink the lock file: waiters must all lock the same inode.
pub(super) struct Ownership {
    paths: Paths,
    lock: File,
    /// The listener a handoff passed over, until `listen` adopts it.
    inherited: Option<StdListener>,
}

impl Ownership {
    /// Lock the state directory, taking over a handoff's lock and listener
    /// or opening the lock file and clearing a stale socket.
    pub(super) fn acquire(paths: &Paths, managed: bool, handoff: Option<Handoff>) -> Result<Self> {
        let (lock, inherited) = match handoff {
            Some(handoff) => {
                ensure!(managed, "daemon handoff requires managed mode");
                handoff.verify(paths)?;
                (handoff.lock, Some(handoff.listener))
            }
            None => (open_lock(paths)?, None),
        };
        lock.try_lock_exclusive()
            .context("a daemon already owns this state directory")?;
        if inherited.is_none() {
            remove_stale_socket(paths)?;
        }
        Ok(Self {
            paths: paths.clone(),
            lock,
            inherited,
        })
    }

    /// Accept clients on the inherited listener or a newly bound socket.
    pub(super) fn listen(&mut self) -> Result<UnixListener> {
        let listener = match self.inherited.take() {
            Some(listener) => UnixListener::from_std(listener).context("inherit daemon socket")?,
            None => UnixListener::bind(&self.paths.socket).context("bind daemon socket")?,
        };
        fs::set_permissions(&self.paths.socket, fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }

    /// Replace this process with the daemon at `path`, passing it the lock
    /// and listener; returns only when exec fails.
    pub(super) fn hand_off(&self, path: &Path, listener: &UnixListener) -> Result<()> {
        handoff::exec(path, &self.paths, listener, &self.lock)
    }
}

impl Drop for Ownership {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.paths.socket);
    }
}

fn open_lock(paths: &Paths) -> Result<File> {
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(paths.daemon_lock())?)
}

fn remove_stale_socket(paths: &Paths) -> Result<()> {
    match fs::symlink_metadata(&paths.socket) {
        Ok(meta) => {
            ensure!(
                meta.file_type().is_socket(),
                "refusing to replace non-socket {}",
                paths.socket.display()
            );
            fs::remove_file(&paths.socket)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
