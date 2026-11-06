//! The handoff of a chat that a process starts in a background place: the
//! starter waits until the place reports that the chat is about to run its
//! agent, or why it does not, since opening the place proves neither. A
//! scoped starter's place records the chat with the starter's scope token,
//! which may end with the starter, so that token goes through the handoff
//! too. The handoff is a private directory that the place's command names,
//! never the place's environment, which a shell left there after the chat
//! would keep.
use std::{
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail, ensure};

/// The file holding the starter's scope token, which the place takes.
const SCOPE: &str = "scope";
/// The file in which the place reports.
const REPORT: &str = "report";
/// What the place reports once the chat holds its own scope token.
const STARTED: &str = "started";
/// How long the starter waits for the place to report.
const WAIT: Duration = Duration::from_secs(60);
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
        if let Some(token) = token {
            std::fs::write(directory.path().join(SCOPE), token)
                .context("write the chat's handoff")?;
        }
        Ok(Self(directory))
    }

    /// Where the place finds the handoff.
    pub fn path(&self) -> &Path {
        self.0.path()
    }

    /// Wait until the place reports, returning why the chat did not start
    /// as the error.
    pub async fn wait(self) -> Result<()> {
        let deadline = Instant::now() + WAIT;
        let report = self.0.path().join(REPORT);
        loop {
            match std::fs::read_to_string(&report) {
                Ok(text) if text == STARTED => return Ok(()),
                Ok(error) => bail!("{error}"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => ensure!(
                    Instant::now() < deadline,
                    "the chat did not start in its background place within {} seconds",
                    WAIT.as_secs()
                ),
                Err(error) => return Err(error).context("read the chat's handoff"),
            }
            tokio::time::sleep(POLL).await;
        }
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
    let scope = path.join(SCOPE);
    let token = match std::fs::read_to_string(&scope) {
        Ok(token) => token,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("read the chat's handoff"),
    };
    let _ = std::fs::remove_file(&scope);
    ensure!(
        crate::env::take_handed_scope(token),
        "this process already holds a handed scope token"
    );
    Ok(())
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
