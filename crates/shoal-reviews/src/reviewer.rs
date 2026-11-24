//! One reviewer run: an AI tool's review command run once in the review's
//! checkout as a one-shot run, under a scope of the reviewed workspace, and
//! how it ended: what it wrote through Shoal's commands, a review or an
//! answer in a finding's thread, or the failure its result names.
use std::{fs, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use tokio::sync::watch;

use crate::{
    log,
    oneshot::{self, AgentCommand},
    reviews::{
        FailureReason, ReviewId,
        output::{self, ResultFormat},
    },
};
use shoal_ids::agent_session_ids::AgentSessionId;

/// A reviewer still running after this long is stopped. Fixed, since a
/// review that long has stopped making progress.
pub const TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The files of one review's run, in its own directory.
#[derive(Clone)]
pub struct Files {
    pub directory: PathBuf,
}

impl Files {
    /// The review whose run these are, named in its [`Self::id`] file.
    pub fn review(&self) -> Option<ReviewId> {
        fs::read_to_string(self.id()).ok()?.trim().parse().ok()
    }

    /// The ID of the review whose run these are, written first, since the
    /// directory names the review's subject.
    pub fn id(&self) -> PathBuf {
        self.directory.join("review")
    }

    pub fn checkout(&self) -> PathBuf {
        self.directory.join("checkout")
    }

    /// Where a reviewer whose arguments name `{output}` writes its result.
    pub fn output(&self) -> PathBuf {
        self.directory.join("result.json")
    }

    /// Remove the result a reviewer wrote to [`Self::output`].
    fn remove_output(&self) {
        let _ = fs::remove_file(self.output());
    }
}

/// What one reviewer runs with.
pub struct Reviewer<'a> {
    pub runs: &'a oneshot::Runs,
    pub command: AgentCommand,
    pub files: &'a Files,
    /// [`TIMEOUT`], but in tests.
    pub timeout: Duration,
    /// Whether the result goes to [`Files::output`] instead of stdout.
    pub writes_output: bool,
    pub format: &'static ResultFormat,
}

/// How a run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// It produced what it was asked for, with the session the reviewer
    /// reported.
    Returned {
        session: Option<AgentSessionId>,
    },
    Failed(FailureReason),
    Cancelled,
    /// What the reviewer started could not be proven stopped, so its run
    /// stays tracked.
    LeftRunning,
}

impl Ended {
    /// This end without the session its reviewer reported, for a run whose
    /// session no later review can find.
    pub fn without_session(self) -> Self {
        match self {
            Self::Returned { .. } => Self::Returned { session: None },
            ended => ended,
        }
    }
}

/// How a run that `produced` what it was asked for, or did not, ended: a
/// run that submitted its review or posted its answer produced it however
/// it exited, and one that exited cleanly without doing so failed.
pub fn settled(ended: Ended, produced: bool) -> Ended {
    match (ended, produced) {
        (Ended::Failed(_), true) => Ended::Returned { session: None },
        (Ended::Returned { .. }, false) => Ended::Failed(FailureReason::NotSubmitted),
        (ended, _) => ended,
    }
}

/// Run `reviewer` until it exits, its timeout passes or `cancel` turns true,
/// and read how it ended from its result, which another reviewer of the run
/// does not see.
pub async fn run(reviewer: &Reviewer<'_>, cancel: &mut watch::Receiver<bool>) -> Ended {
    let files = reviewer.files;
    let run = oneshot::Run {
        runs: reviewer.runs,
        command: &reviewer.command,
        directory: &files.checkout(),
        files: &files.directory,
        timeout: reviewer.timeout,
    };
    let ended = ended(reviewer, oneshot::run(&run, cancel).await);
    reviewer.files.remove_output();
    ended
}

/// What `reviewer`'s run, which ended as `exited`, produced.
fn ended(reviewer: &Reviewer<'_>, exited: Result<oneshot::Ended>) -> Ended {
    match exited {
        Ok(oneshot::Ended::Exited { success }) => match read(reviewer, success) {
            Ok(Ok(session)) => Ended::Returned { session },
            Ok(Err(reason)) => Ended::Failed(reason),
            Err(error) => {
                log!("reading a reviewer's output: {error:#}");
                Ended::Failed(FailureReason::InvalidOutput)
            }
        },
        Ok(oneshot::Ended::Cancelled) => Ended::Cancelled,
        Ok(oneshot::Ended::TimedOut) => Ended::Failed(FailureReason::TimedOut),
        Err(error) if error.downcast_ref::<oneshot::LeftRunning>().is_some() => {
            log!("ending a reviewer: {error:#}");
            Ended::LeftRunning
        }
        Err(error) => {
            log!("running a reviewer: {error:#}");
            Ended::Failed(FailureReason::ExecutionFailed)
        }
    }
}

/// The session the run that exited with `success` reported, or the
/// failure its result or diagnostics name.
fn read(
    reviewer: &Reviewer,
    success: bool,
) -> Result<Result<Option<AgentSessionId>, FailureReason>> {
    let files = &reviewer.files.directory;
    // A result too large to name a failure or a session is never loaded.
    let result = match reviewer.writes_output {
        true => oneshot::read_bounded(&reviewer.files.output(), RESULT_LIMIT)?,
        false => oneshot::read_bounded(&oneshot::stdout(files), RESULT_LIMIT)?,
    };
    let stderr = oneshot::stderr(files);
    let head = oneshot::read_head(&stderr, DIAGNOSTIC_LIMIT)?;
    let tail = oneshot::read_tail(&stderr, DIAGNOSTIC_LIMIT)?;
    let stdout = oneshot::read_tail(&oneshot::stdout(files), DIAGNOSTIC_LIMIT)?;
    let run = output::Run {
        success,
        result: result.as_deref(),
        diagnostics: [&tail, &stdout],
    };
    Ok(output::outcome(reviewer.format, &run)
        .map(|()| output::session(reviewer.format, result.as_deref(), &head)))
}

/// The most of a reviewer's result that is read.
const RESULT_LIMIT: u64 = 8 * 1024 * 1024;

/// The most of each diagnostic stream that is read, from its end, where
/// a failure is reported, and of standard error from its start, where a
/// tool names its session.
const DIAGNOSTIC_LIMIT: u64 = 64 * 1024;

/// Write the ID of review `id`, whose run these files are.
pub fn write_id(files: &Files, id: ReviewId) -> Result<()> {
    fs::write(files.id(), id.to_string()).with_context(|| format!("write {}", files.id().display()))
}

#[cfg(test)]
mod tests;
