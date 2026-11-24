//! What a reviewer's run returns, as the review bot reads it: the failure
//! its result or diagnostics name, the answer it gives in a finding's
//! thread, as the answer's schema describes it, the structured result the
//! gate returns, and the session the run reports.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shoal_protocol::reviews::FailureReason;

use super::failure::{
    SCAN_LIMIT, classify_diagnostic, classify_envelope, classify_text, tail, truthy,
};

/// The JSON schema of [`Answer`], which a reviewer answering a post in a
/// finding's thread is given.
pub const ANSWER_SCHEMA: &str = include_str!("answer_schema.json");

/// A reviewer's reply to the latest post in a finding's thread, as
/// [`ANSWER_SCHEMA`] describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub answer: String,
}

/// How an AI tool's integration hands back a run's result, which
/// [`outcome`] and [`result`] unwrap.
#[derive(Debug)]
pub struct ResultFormat {
    /// The failure a successful run's unparseable result text names.
    unparsed: fn(&str) -> Option<FailureReason>,
    /// The failure a parsed result names besides its error fields.
    failed: fn(&Value) -> Option<FailureReason>,
    /// The structured result in a parsed one.
    structured: fn(Value) -> Result<Value, FailureReason>,
    /// The session the run reports: in its parsed result, or in the head
    /// of its standard error.
    session: fn(Option<&Value>, &str) -> Option<String>,
}

/// Claude Code's JSON result envelope, which carries the structured
/// output. A usage-limit reply can come back as plain text with exit status
/// zero.
pub static CLAUDE: ResultFormat = ResultFormat {
    unparsed: classify_text,
    failed: |raw| {
        (raw.get("is_error").is_some_and(truthy)).then_some(FailureReason::ExecutionFailed)
    },
    structured: |raw| {
        let envelope = raw.as_object().ok_or(FailureReason::InvalidOutput)?;
        match envelope.get("structured_output") {
            Some(review) if !review.is_null() => Ok(review.clone()),
            _ => envelope
                .get("result")
                .and_then(Value::as_str)
                .and_then(|result| serde_json::from_str(result).ok())
                .ok_or(FailureReason::InvalidOutput),
        }
    },
    session: |raw, _| {
        let session = raw?.as_object()?.get("session_id")?.as_str()?;
        Some(session.to_owned())
    },
};

/// The structured output as the whole result, from a tool that reports no
/// session.
pub static PLAIN: ResultFormat = ResultFormat {
    unparsed: |_| None,
    failed: |_| None,
    structured: Ok,
    session: |_, _| None,
};

/// The structured output as the whole result, as Codex writes its last
/// message, which names its session in the header it writes to standard
/// error.
pub static CODEX: ResultFormat = ResultFormat {
    session: |_, stderr| {
        let line = stderr
            .lines()
            .find_map(|line| line.strip_prefix("session id:"))?;
        Some(line.trim().to_owned()).filter(|session| !session.is_empty())
    },
    ..PLAIN
};

/// What a reviewer's run left to read.
pub struct Run<'a> {
    /// Whether it exited successfully.
    pub success: bool,
    /// Its result, from the output file or standard output; `None` when it
    /// could not be read.
    pub result: Option<&'a [u8]>,
    /// Its standard error, then its standard output.
    pub diagnostics: [&'a [u8]; 2],
}

/// Why `run` failed in `format`, if it did: a known failure in its result's
/// error fields, then an unsuccessful exit named by its diagnostics, then a
/// failure its result names otherwise.
pub fn outcome(format: &ResultFormat, run: &Run) -> Result<(), FailureReason> {
    checked(format, run).map(drop)
}

/// The answer `run` returned in `format`, or why it returned none, as
/// [`result`] tells.
pub fn read_answer(format: &ResultFormat, run: &Run) -> Result<Answer, FailureReason> {
    let answer = serde_json::from_value::<Answer>(result(format, run)?).ok();
    (answer.filter(|answer| !answer.answer.trim().is_empty())).ok_or(FailureReason::InvalidOutput)
}

/// The structured result `run` returned in `format`, unchecked against any
/// schema, or why it returned none: the failure [`outcome`] tells, else a
/// result that is not structured.
pub fn result(format: &ResultFormat, run: &Run) -> Result<Value, FailureReason> {
    let raw = checked(format, run)?.ok_or(FailureReason::InvalidOutput)?;
    (format.structured)(raw)
}

/// The parsed result of `run` in `format`, none for one that is not JSON,
/// or the failure [`outcome`] tells.
fn checked(format: &ResultFormat, run: &Run) -> Result<Option<Value>, FailureReason> {
    let raw = run
        .result
        .and_then(|result| serde_json::from_slice::<Value>(result).ok());
    if let Some(reason) = raw.as_ref().and_then(classify_envelope) {
        return Err(reason);
    }
    if !run.success {
        let unparsed = run.result.filter(|_| raw.is_none());
        let reason = unparsed
            .into_iter()
            .chain(run.diagnostics)
            .find_map(|stream| classify_diagnostic(&tail(stream, SCAN_LIMIT)));
        return Err(reason.unwrap_or(FailureReason::ExecutionFailed));
    }
    match raw {
        Some(raw) => match (format.failed)(&raw) {
            Some(reason) => Err(reason),
            None => Ok(Some(raw)),
        },
        None => {
            let text = run.result.map(|result| tail(result, SCAN_LIMIT));
            match text.as_deref().and_then(format.unparsed) {
                Some(reason) => Err(reason),
                None => Ok(None),
            }
        }
    }
}

/// The session the run reported in `format`, from its `result` or the head
/// of its standard error; none from a tool that reports none.
pub fn session(
    format: &ResultFormat,
    result: Option<&[u8]>,
    stderr_head: &[u8],
) -> Option<shoal_ids::agent_session_ids::AgentSessionId> {
    let raw = result.and_then(|result| serde_json::from_slice::<Value>(result).ok());
    let session = (format.session)(raw.as_ref(), &String::from_utf8_lossy(stderr_head))?;
    Some(session.into())
}

#[cfg(test)]
mod tests;
