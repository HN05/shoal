//! What a reviewer returns: the output schema it is given and the result
//! read back from its run, as the review bot reads it, and the session the
//! run reports.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    FailureReason, FindingId, FixCheck, Severity, Verdict,
    failure::{SCAN_LIMIT, classify_diagnostic, classify_envelope, classify_text, tail, truthy},
};

/// The JSON schema of [`ReviewerOutput`], which the reviewer is given.
pub const SCHEMA: &str = include_str!("schema.json");

/// A reviewer's result, as the output schema describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerOutput {
    pub summary: String,
    pub verdict: Verdict,
    pub findings: Vec<NewFinding>,
    /// What it found of each earlier finding resolved `fixed`.
    pub checked: Vec<Checked>,
}

/// A finding as the reviewer reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewFinding {
    pub path: String,
    pub line: u32,
    pub severity: Severity,
    pub title: String,
    pub body: String,
}

/// The reviewer's check of an earlier finding resolved `fixed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checked {
    pub finding: FindingId,
    pub result: FixCheck,
}

/// How an AI tool's integration hands back the reviewer's result, which
/// [`read`] unwraps.
#[derive(Debug)]
pub struct ResultFormat {
    /// The failure a successful run's unparseable result text names.
    unparsed: fn(&str) -> Option<FailureReason>,
    /// The review in a parsed result.
    review: fn(Value) -> Result<Value, FailureReason>,
    /// The session the run reports: in its parsed result, or in the head
    /// of its standard error.
    session: fn(Option<&Value>, &str) -> Option<String>,
}

/// Claude Code's JSON result envelope, whose structured output is the
/// review. A usage-limit reply can come back as plain text with exit status
/// zero.
pub static CLAUDE: ResultFormat = ResultFormat {
    unparsed: classify_text,
    review: |raw| {
        let envelope = raw.as_object().ok_or(FailureReason::InvalidOutput)?;
        if envelope.get("is_error").is_some_and(truthy) {
            return Err(FailureReason::ExecutionFailed);
        }
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

/// The review as the whole result, from a tool that reports no session.
pub static PLAIN: ResultFormat = ResultFormat {
    unparsed: |_| None,
    review: Ok,
    session: |_, _| None,
};

/// The review as the whole result, as Codex writes its last message, which
/// names its session in the header it writes to standard error.
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

/// The review `run` returned in `format`, or why it returned none: a known
/// failure in its result's error fields, then an unsuccessful exit named
/// by its diagnostics, then output that does not match the schema.
pub fn read(format: &ResultFormat, run: &Run) -> Result<ReviewerOutput, FailureReason> {
    validate(result(format, run)?).ok_or(FailureReason::InvalidOutput)
}

/// The structured result `run` returned in `format`, unchecked against any
/// schema, or why it returned none, as [`read`] tells.
pub fn result(format: &ResultFormat, run: &Run) -> Result<Value, FailureReason> {
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
    let Some(raw) = raw else {
        let text = run.result.map(|result| tail(result, SCAN_LIMIT));
        let reason = text.as_deref().and_then(format.unparsed);
        return Err(reason.unwrap_or(FailureReason::InvalidOutput));
    };
    (format.review)(raw)
}

/// The session the run reported in `format`, from its `result` or the head
/// of its standard error; none from a tool that reports none.
pub fn session(
    format: &ResultFormat,
    result: Option<&[u8]>,
    stderr_head: &[u8],
) -> Option<crate::agent::AgentSessionId> {
    let raw = result.and_then(|result| serde_json::from_slice::<Value>(result).ok());
    let session = (format.session)(raw.as_ref(), &String::from_utf8_lossy(stderr_head))?;
    Some(session.into())
}

/// The review in `value` if it matches the schema, with every text present.
fn validate(value: Value) -> Option<ReviewerOutput> {
    let output: ReviewerOutput = serde_json::from_value(value).ok()?;
    let present = |text: &str| !text.trim().is_empty();
    let findings = output.findings.iter().all(|finding| {
        finding.line > 0
            && [&finding.path, &finding.title, &finding.body]
                .into_iter()
                .all(|text| present(text))
    });
    (present(&output.summary) && findings).then_some(output)
}

#[cfg(test)]
mod tests;
