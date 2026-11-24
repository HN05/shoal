use super::*;

const NOW: UnixTime = UnixTime::from_seconds(1_000_000);

fn after(seconds: u64) -> Option<UnixTime> {
    Some(NOW + Duration::from_secs(seconds))
}

#[test]
fn a_duration_after_a_phrase_is_the_reset() {
    for (text, reset) in [
        (
            "API Error: Request rejected (429) · All credentials for model claude-opus-5-5 \
             are cooling down (last error: rate_limit_error: ...) (reset after 14m40s)",
            after(14 * 60 + 40),
        ),
        ("Usage limit reached. Resets in 1h36m24s.", after(5784)),
        ("Please try again in 300s", after(300)),
        ("Retry after 2h", after(7200)),
        ("retry after 1500ms", after(1)),
    ] {
        assert_eq!(stated(text, NOW), reset, "{text}");
    }
}

#[test]
fn a_json_error_states_the_reset_in_seconds_or_as_a_time() {
    let nested = r#"429 {"error":{"type":"rate_limit_error","resets_in_seconds":90}}"#;
    assert_eq!(stated(nested, NOW), after(90));
    let seconds = r#"upstream said {"reset_seconds": 600, "message": "slow down"}"#;
    assert_eq!(stated(seconds, NOW), after(600));
    let at = r#"{"type":"error","error":{"resets_at":1700000000}}"#;
    assert_eq!(stated(at, NOW), Some(UnixTime::from_seconds(1_700_000_000)));
}

#[test]
fn text_without_a_reset_states_none() {
    for text in [
        "API Error: 529 Overloaded",
        "rate limited; try again in a few minutes",
        "retry after 5seconds",
        r#"{"error":{"type":"rate_limit_error"}} {not json"#,
    ] {
        assert_eq!(stated(text, NOW), None, "{text}");
    }
}
