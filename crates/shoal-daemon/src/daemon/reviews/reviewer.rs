//! One reviewer run: an AI tool's review command run once in the review's
//! checkout as a one-shot run, and its result read back.
use std::{fs, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use tokio::sync::watch;

use crate::{
    daemon::oneshot::{self, AgentCommand},
    reviews::{
        FailureReason, ReviewId,
        output::{self, ResultFormat, ReviewerOutput},
    },
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
    pub fn review(&self) -> Option<ReviewId> {
        self.directory.file_name()?.to_str()?.parse().ok()
    }

    pub fn checkout(&self) -> PathBuf {
        self.directory.join("checkout")
    }

    /// The ID of the checkout's repository, written before the checkout,
    /// so a restart finds it without the review.
    pub fn repository(&self) -> PathBuf {
        self.directory.join("repository")
    }

    pub fn schema(&self) -> PathBuf {
        self.directory.join("schema.json")
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
pub(super) struct Reviewer<'a> {
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
pub(super) enum Ended {
    Returned(ReviewerOutput),
    Failed(FailureReason),
    Cancelled,
    /// What the reviewer started could not be proven stopped, so its run
    /// stays tracked.
    LeftRunning,
}

/// Run `reviewer` until it exits, its timeout passes or `cancel` turns true,
/// and read its result, which another reviewer of the run does not see.
pub(super) async fn run(reviewer: &Reviewer<'_>, cancel: &mut watch::Receiver<bool>) -> Ended {
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
            Ok(Ok(output)) => Ended::Returned(output),
            Ok(Err(reason)) => Ended::Failed(reason),
            Err(error) => {
                crate::daemon::log!("reading a reviewer's output: {error:#}");
                Ended::Failed(FailureReason::InvalidOutput)
            }
        },
        Ok(oneshot::Ended::Cancelled) => Ended::Cancelled,
        Ok(oneshot::Ended::TimedOut) => Ended::Failed(FailureReason::TimedOut),
        Err(error) if error.downcast_ref::<oneshot::LeftRunning>().is_some() => {
            crate::daemon::log!("ending a reviewer: {error:#}");
            Ended::LeftRunning
        }
        Err(error) => {
            crate::daemon::log!("running a reviewer: {error:#}");
            Ended::Failed(FailureReason::ExecutionFailed)
        }
    }
}

fn read(reviewer: &Reviewer, success: bool) -> Result<Result<ReviewerOutput, FailureReason>> {
    let files = &reviewer.files.directory;
    // A result too large to be a review is no review; it is never loaded.
    let result = match reviewer.writes_output {
        true => oneshot::read_bounded(&reviewer.files.output(), RESULT_LIMIT)?,
        false => oneshot::read_bounded(&oneshot::stdout(files), RESULT_LIMIT)?,
    };
    let stderr = oneshot::read_tail(&oneshot::stderr(files), DIAGNOSTIC_LIMIT)?;
    let stdout = oneshot::read_tail(&oneshot::stdout(files), DIAGNOSTIC_LIMIT)?;
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

/// Write the output schema the reviewer is given.
pub(super) fn write_schema(files: &Files) -> Result<()> {
    fs::write(files.schema(), output::SCHEMA)
        .with_context(|| format!("write {}", files.schema().display()))
}

#[cfg(test)]
mod tests;
