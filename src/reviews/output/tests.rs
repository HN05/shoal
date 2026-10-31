use serde_json::json;

use super::*;

fn review() -> Value {
    json!({
        "summary": "One problem.",
        "verdict": "comment",
        "findings": [{
            "path": "src/a.rs", "line": 7, "severity": "should-fix",
            "title": "Unchecked index", "body": "It panics on an empty list."
        }],
        "checked": [{"finding": "3.1", "result": "confirmed"}]
    })
}

fn run(
    success: bool,
    result: Option<&[u8]>,
    stderr: &[u8],
) -> Result<ReviewerOutput, FailureReason> {
    run_as(&PLAIN, success, result, stderr)
}

fn run_as(
    format: &ResultFormat,
    success: bool,
    result: Option<&[u8]>,
    stderr: &[u8],
) -> Result<ReviewerOutput, FailureReason> {
    read(
        format,
        &Run {
            success,
            result,
            diagnostics: [stderr, b""],
        },
    )
}

#[test]
fn the_schema_describes_the_output() {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key.as_str().unwrap())
        .collect();
    assert_eq!(required, ["summary", "verdict", "findings", "checked"]);
    let output = run(true, Some(review().to_string().as_bytes()), b"").unwrap();
    assert_eq!(output.findings[0].line, 7);
    assert_eq!(output.checked[0].finding.to_string(), "3.1");
}

#[test]
fn output_off_the_schema_is_invalid() {
    let mut extra = review();
    extra["extra"] = json!(1);
    let mut empty_title = review();
    empty_title["findings"][0]["title"] = json!(" ");
    let mut line_zero = review();
    line_zero["findings"][0]["line"] = json!(0);
    let mut severity = review();
    severity["findings"][0]["severity"] = json!("major");
    for value in [extra, empty_title, line_zero, severity] {
        let text = value.to_string();
        assert_eq!(
            run(true, Some(text.as_bytes()), b""),
            Err(FailureReason::InvalidOutput),
            "{text}"
        );
    }
    assert_eq!(run(true, None, b""), Err(FailureReason::InvalidOutput));
    assert_eq!(
        run(true, Some(b"not json"), b""),
        Err(FailureReason::InvalidOutput)
    );
}

#[test]
fn findings_and_any_verdict_are_results() {
    let mut blocking = review();
    blocking["verdict"] = json!("request_changes");
    blocking["findings"][0]["severity"] = json!("blocker");
    let output = run(true, Some(blocking.to_string().as_bytes()), b"").unwrap();
    assert_eq!(output.verdict, Verdict::RequestChanges);
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
    // An unsuccessful exit is a failure even with a review written.
    assert_eq!(
        run(false, Some(review().to_string().as_bytes()), b""),
        Err(FailureReason::ExecutionFailed)
    );
}

#[test]
fn claude_envelopes_carry_the_review_or_an_error() {
    let envelope =
        json!({"type": "result", "is_error": false, "result": "", "structured_output": review()});
    let read = |value: &Value| run_as(&CLAUDE, true, Some(value.to_string().as_bytes()), b"");
    assert!(read(&envelope).is_ok());
    let in_result = json!({"type": "result", "result": review().to_string()});
    assert!(read(&in_result).is_ok());
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
