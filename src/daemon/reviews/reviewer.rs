//! One reviewer run: an AI tool's review command in the review's checkout,
//! in a process group of its own that is killed when the run is stopped,
//! and its result read back.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    path::PathBuf,
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::{process::Command, sync::watch};

use crate::reviews::{
    FailureReason,
    output::{self, ResultFormat, ReviewerOutput},
};

/// A reviewer still running after this long is stopped. Fixed, since a
/// review that long has stopped making progress.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The files of one review's run, in its own directory.
pub(super) struct Files {
    pub directory: PathBuf,
}

impl Files {
    /// The review whose run these are, named by their directory.
    pub fn review(&self) -> Option<i64> {
        self.directory.file_name()?.to_str()?.parse().ok()
    }

    pub fn checkout(&self) -> PathBuf {
        self.directory.join("checkout")
    }

    pub fn schema(&self) -> PathBuf {
        self.directory.join("schema.json")
    }

    /// Where a reviewer whose arguments name `{output}` writes its result.
    pub fn output(&self) -> PathBuf {
        self.directory.join("result.json")
    }

    fn stdout(&self) -> PathBuf {
        self.directory.join("stdout")
    }

    fn stderr(&self) -> PathBuf {
        self.directory.join("stderr")
    }
}

/// What one reviewer runs with.
pub(super) struct Reviewer<'a> {
    pub argv: Vec<OsString>,
    /// Variables added to the daemon's own, without its Shoal variables.
    pub environment: BTreeMap<String, OsString>,
    pub remove: BTreeSet<String>,
    pub files: &'a Files,
    /// Whether the result goes to [`Files::output`] instead of stdout.
    pub writes_output: bool,
    pub format: &'static ResultFormat,
}

/// How a run ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ended {
    Returned(ReviewerOutput),
    Failed(FailureReason),
    Cancelled,
}

/// Run `reviewer` until it exits, [`TIMEOUT`] passes or `cancel` turns true,
/// and read its result.
pub(super) async fn run(reviewer: &Reviewer<'_>, cancel: &mut watch::Receiver<bool>) -> Ended {
    match exit(reviewer, cancel).await {
        Ok(Some(success)) => match read(reviewer, success) {
            Ok(Ok(output)) => Ended::Returned(output),
            Ok(Err(reason)) => Ended::Failed(reason),
            Err(error) => {
                crate::daemon::log!("reading a reviewer's output: {error:#}");
                Ended::Failed(FailureReason::InvalidOutput)
            }
        },
        Ok(None) => Ended::Cancelled,
        Err(Stopped::TimedOut) => Ended::Failed(FailureReason::TimedOut),
        Err(Stopped::Failed(error)) => {
            crate::daemon::log!("running a reviewer: {error:#}");
            Ended::Failed(FailureReason::ExecutionFailed)
        }
    }
}

enum Stopped {
    TimedOut,
    Failed(anyhow::Error),
}

/// Whether the reviewer exited successfully, or `None` when cancelled.
async fn exit(
    reviewer: &Reviewer<'_>,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Option<bool>, Stopped> {
    let mut child = command(reviewer)
        .and_then(|mut command| command.spawn().context("start the reviewer"))
        .map_err(Stopped::Failed)?;
    let group = child
        .id()
        .context("reviewer process ID unavailable")
        .map_err(Stopped::Failed)? as i32;
    // Dropping the run, as a daemon stop does, kills its whole group.
    let mut guard = GroupGuard(Some(group));
    // The leader stays unreaped until its group is killed, so its ID names
    // this group alone when the signal is sent.
    let ended = tokio::select! {
        exited = tokio::time::timeout(TIMEOUT, exited_unreaped(group)) => match exited {
            Ok(exited) => exited.map(|()| true).map_err(|error| Stopped::Failed(error.into())),
            Err(_) => Err(Stopped::TimedOut),
        },
        _ = cancel.wait_for(|cancelled| *cancelled) => Ok(false),
    };
    // SAFETY: killpg takes no pointers; a group without members is ESRCH.
    unsafe { libc::killpg(group, libc::SIGKILL) };
    let status = child.wait().await;
    // Once reaped, the ID may name another group.
    guard.0 = None;
    match ended? {
        true => Ok(Some(
            status
                .map_err(|error| Stopped::Failed(error.into()))?
                .success(),
        )),
        false => Ok(None),
    }
}

/// How often a running reviewer is checked for having exited.
const EXIT_POLL: Duration = Duration::from_millis(100);

/// Wait for the child `pid` to exit without reaping it, so its ID still
/// names its process group until the caller reaps it. It polls rather than
/// blocking a thread, so a paused clock can drive the timeout in tests.
async fn exited_unreaped(pid: i32) -> std::io::Result<()> {
    while !has_exited(pid)? {
        tokio::time::sleep(EXIT_POLL).await;
    }
    Ok(())
}

/// Whether the child `pid` has exited, leaving it unreaped.
fn has_exited(pid: i32) -> std::io::Result<bool> {
    loop {
        // SAFETY: `info` is a valid, writable siginfo_t for waitid to fill.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let flags = libc::WEXITED | libc::WNOWAIT | libc::WNOHANG;
        if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) } == 0 {
            // SAFETY: waitid filled `info`; without an exited child it zeroes the PID.
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// The reviewer as its own process group in the checkout, with the
/// configured variables and none of the daemon's Shoal variables or secrets.
fn command(reviewer: &Reviewer) -> Result<Command> {
    let (program, args) = reviewer
        .argv
        .split_first()
        .context("empty review command")?;
    let files = reviewer.files;
    let mut command = Command::new(program);
    command.args(args);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with(crate::env::PREFIX) {
            command.env_remove(name);
        }
    }
    for name in crate::env::inherited_secret_exports() {
        command.env_remove(name);
    }
    if let Some(path) = crate::env::path_without_secret_tools() {
        command.env("PATH", path);
    }
    crate::env::apply_configured(&mut command, &reviewer.environment);
    for name in &reviewer.remove {
        command.env_remove(name);
    }
    command.env(crate::env::REVIEW_RUN, &files.directory);
    command
        .current_dir(files.checkout())
        .stdin(Stdio::null())
        .stdout(fs::File::create(files.stdout())?)
        .stderr(fs::File::create(files.stderr())?)
        .process_group(0)
        .kill_on_drop(true);
    Ok(command)
}

fn read(reviewer: &Reviewer, success: bool) -> Result<Result<ReviewerOutput, FailureReason>> {
    let files = reviewer.files;
    // A result too large to be a review is no review; it is never loaded.
    let result = match reviewer.writes_output {
        true => read_bounded(&files.output(), RESULT_LIMIT)?,
        false => read_bounded(&files.stdout(), RESULT_LIMIT)?,
    };
    let stderr = read_tail(&files.stderr(), DIAGNOSTIC_LIMIT)?;
    let stdout = read_tail(&files.stdout(), DIAGNOSTIC_LIMIT)?;
    Ok(output::read(
        reviewer.format,
        &output::Run {
            success,
            result: result.as_deref(),
            diagnostics: [&stderr, &stdout],
        },
    ))
}

/// The most of a reviewer's result that is read.
const RESULT_LIMIT: u64 = 8 * 1024 * 1024;

/// The most of each diagnostic stream that is read, from its end, where
/// a failure is reported.
const DIAGNOSTIC_LIMIT: u64 = 64 * 1024;

/// The file's bytes, unless it is missing, as from a reviewer that wrote
/// none, or larger than `limit`.
fn read_bounded(path: &std::path::Path, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > limit => Ok(None),
        Ok(_) => fs::read(path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The last `limit` bytes of the file.
fn read_tail(path: &std::path::Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(limit)))?;
    let mut tail = Vec::new();
    file.take(limit).read_to_end(&mut tail)?;
    Ok(tail)
}

/// Kills a reviewer's process group when dropped before its leader is
/// reaped, while the ID still names that group alone.
struct GroupGuard(Option<i32>);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            // SAFETY: killpg takes no pointers.
            unsafe { libc::killpg(group, libc::SIGKILL) };
        }
    }
}

/// Write the output schema the reviewer is given.
pub(super) fn write_schema(files: &Files) -> Result<()> {
    fs::write(files.schema(), output::SCHEMA)
        .with_context(|| format!("write {}", files.schema().display()))
}

#[cfg(test)]
mod tests;
