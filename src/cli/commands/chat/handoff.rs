//! The handoff of a chat that a process starts in a background place: the
//! starter waits until the place reports that the chat is about to run its
//! agent, or why it does not, since opening the place proves neither. Once
//! the place takes the handoff it holds a lock there until it exits, so the
//! starter waits as long as the place runs and learns when it ended without
//! a report; only a place that never takes the handoff meets a limit. A
//! scoped starter's place records the chat with the starter's scope token,
//! which may end with the starter, so that token goes through the handoff
//! too. The handoff is a private directory that the place's command names,
//! never the place's environment, which a shell left there after the chat
//! would keep.
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, bail, ensure};

/// The file holding a scoped starter's token, which the place takes.
const SCOPE: &str = "scope";
/// The file that says the starter is unscoped, so that a handoff missing
/// both, as one whose starter stopped waiting, is refused rather than taken
/// as unscoped.
const UNSCOPED: &str = "unscoped";
/// The file in which the place reports.
const REPORT: &str = "report";
/// What the place reports once the chat holds its own scope token.
const STARTED: &str = "started";
/// The file the place holds locked from taking the handoff until it exits.
const PLACE: &str = "place";
/// How long the starter waits for the place to take the handoff, a hang
/// guard for a place that never runs Shoal, generous since a loaded
/// machine may take minutes to open a place and start Shoal in it.
const PICKUP: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_millis(100);

/// The directory the starter hands the place its scope token in and waits
/// on for the place's report; it goes with the starter.
pub(super) struct Handoff(tempfile::TempDir);

impl Handoff {
    /// A handoff, of the scope token `token` when the starter has one, which
    /// only this user reads.
    pub fn create(token: Option<&str>) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::Builder::new()
            .prefix("shoal-chat.")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .context("create the chat's handoff")?;
        let (file, contents) = match token {
            Some(token) => (SCOPE, token),
            None => (UNSCOPED, ""),
        };
        std::fs::write(directory.path().join(file), contents)
            .context("write the chat's handoff")?;
        Ok(Self(directory))
    }

    /// Where the place finds the handoff.
    pub fn path(&self) -> &Path {
        self.0.path()
    }

    /// Wait until the place reports, returning why the chat did not start
    /// as the error.
    pub async fn wait(self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + PICKUP;
        loop {
            // The place reports before it exits, so a place found ended
            // before the report is read ended without one.
            let place = place(self.0.path())?;
            match reported(self.0.path())? {
                Some(text) if text == STARTED => return Ok(()),
                Some(error) => bail!("{error}"),
                None => {}
            }
            match place {
                Place::Waiting => ensure!(
                    tokio::time::Instant::now() < deadline,
                    "the background place did not run Shoal within {} minutes; run the command again",
                    PICKUP.as_secs() / 60
                ),
                Place::Running => {}
                Place::Ended => bail!(
                    "the background place ended before the chat started; run the command again"
                ),
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

/// What the place of the handoff at `path` reported, if it has.
fn reported(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path.join(REPORT)) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("read the chat's handoff"),
    }
}

/// Where the place of a handoff is.
enum Place {
    /// It has not taken the handoff.
    Waiting,
    /// It took the handoff and runs.
    Running,
    /// It took the handoff and exited.
    Ended,
}

/// Where the place of the handoff at `path` is, by its lock.
fn place(path: &Path) -> Result<Place> {
    let file = match File::open(path.join(PLACE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Place::Waiting),
        Err(error) => return Err(error).context("read the chat's handoff"),
    };
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(Place::Ended),
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Ok(Place::Running)
        }
        Err(error) => Err(error).context("read the chat's handoff"),
    }
}

/// The handoff this process received, to which it reports.
static RECEIVED: OnceLock<PathBuf> = OnceLock::new();

/// Take the starter's scope token from the handoff at `path`, when it handed
/// one, as this process's own, and report to the starter from now on.
pub(super) fn receive(path: &Path) -> Result<()> {
    ensure!(
        RECEIVED.set(path.to_owned()).is_ok(),
        "this process already received a chat's handoff"
    );
    if let Some(token) = take_scope(path)? {
        ensure!(
            crate::env::take_handed_scope(token),
            "this process already holds a handed scope token"
        );
    }
    hold(path)
}

/// Hold the lock that tells the starter at the handoff `path` this place
/// runs, until this process exits. The lock is taken before the file is
/// where the starter looks, so the starter never finds it unheld while
/// this process runs.
fn hold(path: &Path) -> Result<()> {
    static HELD: OnceLock<File> = OnceLock::new();
    let staging = path.join(format!("{PLACE}.staging"));
    let file = File::create(&staging).context("take the chat's handoff")?;
    fs2::FileExt::lock_exclusive(&file).context("take the chat's handoff")?;
    std::fs::rename(&staging, path.join(PLACE)).context("take the chat's handoff")?;
    let _ = HELD.set(file);
    Ok(())
}

/// The scope token the handoff at `path` hands over, which it gives up, or
/// `None` from an unscoped starter. A handoff that says neither, as one
/// whose starter stopped waiting and removed it, is refused, so the chat
/// never starts unscoped in a scoped starter's stead.
fn take_scope(path: &Path) -> Result<Option<String>> {
    let scope = path.join(SCOPE);
    match std::fs::read_to_string(&scope) {
        Ok(token) => {
            let _ = std::fs::remove_file(&scope);
            Ok(Some(token))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure!(
                path.join(UNSCOPED).exists(),
                "the chat's starter no longer waits for it; start the chat again"
            );
            Ok(None)
        }
        Err(error) => Err(error).context("read the chat's handoff"),
    }
}

/// Whether this process is the place a starter waits on.
pub(super) fn awaited() -> bool {
    RECEIVED.get().is_some()
}

/// Report to the starter that waits on this place that the chat started,
/// or why it did not; only the first report counts.
pub(in crate::cli::commands) fn report<T>(outcome: &Result<T>) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    let Some(directory) = RECEIVED.get() else {
        return;
    };
    if REPORTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let text = match outcome {
        Ok(_) => STARTED.to_owned(),
        Err(error) => format!("{error:#}"),
    };
    write_report(directory, &text);
}

/// Leave `text` as the report in the handoff `directory` whole, so the
/// starter reads all of it or none: written beside it, then moved there.
fn write_report(directory: &Path, text: &str) {
    let staging = directory.join(format!("{REPORT}.staging"));
    if std::fs::write(&staging, text).is_ok()
        && std::fs::rename(&staging, directory.join(REPORT)).is_err()
    {
        let _ = std::fs::remove_file(&staging);
    }
}

#[cfg(test)]
mod tests;
