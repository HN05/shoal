use serde_json::json;
use shoal_protocol::reviews::FailureReason;

use super::*;

fn run(success: bool, result: Option<&[u8]>, stderr: &[u8]) -> Result<(), FailureReason> {
    run_as(&PLAIN, success, result, stderr)
}

fn run_as(
    format: &ResultFormat,
    success: bool,
    result: Option<&[u8]>,
    stderr: &[u8],
) -> Result<(), FailureReason> {
    outcome(
        format,
        &Run {
            success,
            result,
            diagnostics: [stderr, b""],
        },
    )
}

#[test]
fn a_successful_exit_is_no_failure_whatever_its_text() {
    assert_eq!(run(true, Some(b"I submitted the review."), b""), Ok(()));
    assert_eq!(run(true, None, b""), Ok(()));
    assert_eq!(run_as(&CODEX, true, Some(b"Done."), b""), Ok(()));
}

#[test]
fn an_unsuccessful_exit_is_named_by_its_diagnostics() {
    assert_eq!(
        run(false, None, b"Error: 429 Too Many Requests"),
        Err(FailureReason::RateLimit)
    );
    assert_eq!(
        run(false, None, b"thread panicked"),
        Err(FailureReason::ExecutionFailed)
    );
    assert_eq!(
        run(false, Some(b"{}"), b""),
        Err(FailureReason::ExecutionFailed)
    );
}

#[test]
fn claude_envelopes_carry_the_result_or_an_error() {
    let read = |value: &Value| run_as(&CLAUDE, true, Some(value.to_string().as_bytes()), b"");
    let done = json!({"type": "result", "is_error": false, "result": "Submitted."});
    assert_eq!(read(&done), Ok(()));
    let failed = json!({"type": "result", "is_error": true, "result": "Something broke"});
    assert_eq!(read(&failed), Err(FailureReason::ExecutionFailed));
    let limited = json!({"type": "result", "is_error": true, "result": "You've hit your limit"});
    assert_eq!(read(&limited), Err(FailureReason::UsageLimit));
    // A plain usage-limit reply with exit status zero.
    assert_eq!(
        run_as(
            &CLAUDE,
            true,
            Some(b"Claude usage limit reached|1750000000"),
            b""
        ),
        Err(FailureReason::UsageLimit)
    );
}

#[test]
fn a_structured_result_is_the_envelopes_or_the_whole_result() {
    let verdict = json!({"verdict": "allow"});
    let read = |format: &ResultFormat, result: &Value| {
        let text = result.to_string();
        let run = Run {
            success: true,
            result: Some(text.as_bytes()),
            diagnostics: [b"", b""],
        };
        super::result(format, &run)
    };
    let envelope = json!({"type": "result", "result": "", "structured_output": verdict});
    assert_eq!(read(&CLAUDE, &envelope), Ok(verdict.clone()));
    let in_result = json!({"type": "result", "result": verdict.to_string()});
    assert_eq!(read(&CLAUDE, &in_result), Ok(verdict.clone()));
    assert_eq!(read(&PLAIN, &verdict), Ok(verdict));
    let text = Run {
        success: true,
        result: Some(b"not json"),
        diagnostics: [b"", b""],
    };
    assert_eq!(
        super::result(&PLAIN, &text),
        Err(FailureReason::InvalidOutput)
    );
}

#[test]
fn a_run_reports_its_session_as_its_tool_does() {
    let envelope = json!({"type": "result", "session_id": "4f2c", "result": "{}"}).to_string();
    let claude = session(&CLAUDE, Some(envelope.as_bytes()), b"");
    assert_eq!(claude.as_ref().map(|id| id.as_str()), Some("4f2c"));
    let header = b"OpenAI Codex\n--------\nworkdir: /r\nsession id: 0199a2\n--------\n";
    let codex = session(&CODEX, Some(b"{}"), header);
    assert_eq!(codex.as_ref().map(|id| id.as_str()), Some("0199a2"));
    assert!(session(&PLAIN, Some(envelope.as_bytes()), header).is_none());
    assert!(session(&CLAUDE, Some(b"not json"), b"").is_none());
}
