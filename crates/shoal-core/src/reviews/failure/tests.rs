use serde_json::json;

use super::*;

#[test]
fn known_phrases_name_their_failure() {
    for (text, reason) in [
        (
            "You've hit your usage limit. Upgrade to Pro",
            FailureReason::UsageLimit,
        ),
        (
            "You’ve reached your Claude limit",
            FailureReason::UsageLimit,
        ),
        (
            "Weekly limit reached · resets 5pm",
            FailureReason::UsageLimit,
        ),
        (
            "Session limits has been exceeded",
            FailureReason::UsageLimit,
        ),
        ("error: usage_limit_reached", FailureReason::UsageLimit),
        (
            "insufficient_quota: check your plan",
            FailureReason::QuotaExhausted,
        ),
        (
            "You exceeded your current quota",
            FailureReason::QuotaExhausted,
        ),
        ("Credit balance is too low", FailureReason::QuotaExhausted),
        ("rate_limit_error", FailureReason::RateLimit),
        ("Too Many Requests", FailureReason::RateLimit),
        ("HTTP error: 429", FailureReason::RateLimit),
        ("status code=429", FailureReason::RateLimit),
        ("request failed (429)", FailureReason::RateLimit),
        (
            "authentication_error: invalid x-api-key",
            FailureReason::AuthenticationFailed,
        ),
        (
            "Please sign in with `claude auth login`",
            FailureReason::AuthenticationFailed,
        ),
    ] {
        assert_eq!(classify_text(text), Some(reason), "{text}");
    }
    for text in [
        "port 4290",
        "status 4291",
        "the reviewer finished",
        "limit the cache",
    ] {
        assert_eq!(classify_text(text), None, "{text}");
    }
}

#[test]
fn envelopes_are_read_from_their_error_fields_only() {
    assert_eq!(
        classify_envelope(&json!({"error": {"type": "rate_limit_error"}})),
        Some(FailureReason::RateLimit)
    );
    assert_eq!(
        classify_envelope(&json!({"is_error": true, "result": "You've hit your limit"})),
        Some(FailureReason::UsageLimit)
    );
    assert_eq!(
        classify_envelope(&json!({"api_error_status": 429, "result": "{}"})),
        Some(FailureReason::RateLimit)
    );
    // A review that discusses rate limits is a review.
    let review = json!({"summary": "Handle the rate limit", "findings": []});
    assert_eq!(
        classify_envelope(&json!({"result": review.to_string(), "structured_output": null})),
        None
    );
    assert_eq!(classify_envelope(&review), None);
}

#[test]
fn diagnostics_count_error_lines_and_events_only() {
    let stderr = "\x1b[31mERROR\x1b[0m: Too many requests\n";
    assert_eq!(classify_diagnostic(stderr), Some(FailureReason::RateLimit));
    let event = r#"{"type":"turn.failed","message":"You've hit your usage limit"}"#;
    assert_eq!(classify_diagnostic(event), Some(FailureReason::UsageLimit));
    let stamped = "2026-10-31T10:00:00Z error: insufficient_quota";
    assert_eq!(
        classify_diagnostic(stamped),
        Some(FailureReason::QuotaExhausted)
    );
    let echoed = "Reviewing the rate limit handling in src/api.rs";
    assert_eq!(classify_diagnostic(echoed), None);
}
