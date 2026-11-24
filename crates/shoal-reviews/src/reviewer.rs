//! One reviewer run: an AI tool's review command run once in the review's
//! checkout as a one-shot run, under a scope of the reviewed workspace, and
//! how it ended: a review it wrote through Shoal's commands, or an answer
//! in a finding's thread, read back.
use std::{ffi::OsString, fs, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use shoal_daemon_base::callers::Access;
use tokio::sync::watch;

use crate::{
    ai::{Arguments, ReviewFields},
    log,
    oneshot::{self, AgentCommand},
    reviews::{
        FailureReason, ReviewId,
        output::{self, Answer, ResultFormat},
    },
};

/// What a reviewer's run returns, read from its result, with the schema it
/// is given, the access its scope gives, the template it runs with and
/// what a session it resumes is told first.
pub trait Output: Sized {
    /// The schema of what it returns, none for a run that writes through
    /// Shoal's commands.
    const SCHEMA: Option<&'static str>;
    const RESUMED: &'static str;

    /// What the scope of a run for review `review` lets it change.
    fn access(review: ReviewId) -> Access;

    /// The arguments of a run with `fields` from `arguments`, resuming
    /// `session` when given, and whether its result goes to `{output}`;
    /// none without the template.
    fn arguments(
        arguments: &Arguments,
        fields: &ReviewFields,
        session: Option<&AgentSessionId>,
    ) -> Option<(Vec<OsString>, bool)>;

    fn read(format: &ResultFormat, run: &output::Run) -> Result<Self, FailureReason>;

    /// How a run of a review ended, given whether the review is `finished`.
    fn settled(ended: Ended<Self>, finished: bool) -> Ended<Self>;
}

/// A review's run that exited without a failure its result or diagnostics
/// name; it writes the review itself, through Shoal's commands.
#[derive(Debug, PartialEq, Eq)]
pub struct Exited;

impl Output for Exited {
    const SCHEMA: Option<&'static str> = None;
    const RESUMED: &'static str = "Earlier in this session you reviewed an earlier commit of \
        this work. Review the commit now checked out as the instructions below say; what you \
        learned before is context, and the history after them says how each earlier finding \
        was resolved.";

    fn access(review: ReviewId) -> Access {
        Access::Review(review)
    }

    fn arguments(
        arguments: &Arguments,
        fields: &ReviewFields,
        session: Option<&AgentSessionId>,
    ) -> Option<(Vec<OsString>, bool)> {
        Some(match session {
            Some(session) => (
                arguments.review_resume_args(fields, session)?,
                arguments.review_resume_writes_output(),
            ),
            None => (
                arguments.review_args(fields)?,
                arguments.review_writes_output(),
            ),
        })
    }

    fn read(format: &ResultFormat, run: &output::Run) -> Result<Self, FailureReason> {
        output::outcome(format, run).map(|()| Exited)
    }

    /// A run that submitted its review produced it, however it exited; one
    /// that exited cleanly without submitting failed.
    fn settled(ended: Ended<Self>, finished: bool) -> Ended<Self> {
        match (ended, finished) {
            (Ended::Failed(_), true) => Ended::Returned {
                output: Exited,
                session: None,
            },
            (Ended::Returned { .. }, false) => Ended::Failed(FailureReason::NotSubmitted),
            (ended, _) => ended,
        }
    }
}

impl Output for Answer {
    const SCHEMA: Option<&'static str> = Some(output::ANSWER_SCHEMA);
    const RESUMED: &'static str = "Earlier in this session you reviewed this work. Answer as \
        the instructions below say; what you learned before is context.";

    fn access(_: ReviewId) -> Access {
        Access::ReadOnly
    }

    /// An answer returns its result in its schema, as the gate's runs do.
    fn arguments(
        arguments: &Arguments,
        fields: &ReviewFields,
        session: Option<&AgentSessionId>,
    ) -> Option<(Vec<OsString>, bool)> {
        Some(match session {
            Some(session) => (
                arguments.gate_resume_args(fields, session)?,
                arguments.gate_resume_writes_output(),
            ),
            None => (arguments.gate_args(fields)?, arguments.gate_writes_output()),
        })
    }

    fn read(format: &ResultFormat, run: &output::Run) -> Result<Self, FailureReason> {
        output::read_answer(format, run)
    }

    fn settled(ended: Ended<Self>, _: bool) -> Ended<Self> {
        ended
    }
}
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
pub enum Ended<O> {
    Returned {
        output: O,
        /// The session the reviewer reported.
        session: Option<AgentSessionId>,
    },
    Failed(FailureReason),
    Cancelled,
    /// What the reviewer started could not be proven stopped, so its run
    /// stays tracked.
    LeftRunning,
}

impl<O> Ended<O> {
    /// This end without the session its reviewer reported, for a run whose
    /// session no later review can find.
    pub fn without_session(self) -> Self {
        match self {
            Self::Returned { output, .. } => Self::Returned {
                output,
                session: None,
            },
            ended => ended,
        }
    }
}

/// Run `reviewer` until it exits, its timeout passes or `cancel` turns true,
/// and read its result, which another reviewer of the run does not see.
pub async fn run<O: Output>(
    reviewer: &Reviewer<'_>,
    cancel: &mut watch::Receiver<bool>,
) -> Ended<O> {
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
fn ended<O: Output>(reviewer: &Reviewer<'_>, exited: Result<oneshot::Ended>) -> Ended<O> {
    match exited {
        Ok(oneshot::Ended::Exited { success }) => match read(reviewer, success) {
            Ok(Ok((output, session))) => Ended::Returned { output, session },
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

/// What the run that exited with `success` returned, with the session it
/// reported, or why it returned none.
fn read<O: Output>(
    reviewer: &Reviewer,
    success: bool,
) -> Result<Result<(O, Option<AgentSessionId>), FailureReason>> {
    let files = &reviewer.files.directory;
    // A result too large to be a review is no review; it is never loaded.
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
    Ok(O::read(reviewer.format, &run).map(|output| {
        let session = output::session(reviewer.format, result.as_deref(), &head);
        (output, session)
    }))
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

/// Write the output schema `schema` the reviewer is given, for a run that
/// returns one.
pub fn write_schema(files: &Files, schema: &str) -> Result<()> {
    fs::write(files.schema(), schema).with_context(|| format!("write {}", files.schema().display()))
}

#[cfg(test)]
mod tests;
