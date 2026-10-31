use super::*;
use crate::transport::response::Response;

fn response(status: u16, headers: &[(&str, &str)]) -> Response {
    Response {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body: String::new(),
    }
}

#[test]
fn refusals_wait_for_retry_after_then_the_reset_then_a_minute() {
    let now = 1_800_000_000;
    assert_eq!(
        limit(&response(429, &[("retry-after", "30")]), now),
        Some(Limit::Refused(now + 30))
    );
    let github = [
        ("x-ratelimit-remaining", "0"),
        ("x-ratelimit-reset", "1800000600"),
    ];
    assert_eq!(
        limit(&response(403, &github), now),
        Some(Limit::Refused(now + 600))
    );
    // GitLab's reset may count seconds from now.
    let gitlab = [("ratelimit-remaining", "0"), ("ratelimit-reset", "45")];
    assert_eq!(
        limit(&response(429, &gitlab), now),
        Some(Limit::Refused(now + 45))
    );
    assert_eq!(
        limit(&response(429, &[]), now),
        Some(Limit::Refused(now + DEFAULT_WAIT))
    );
    assert_eq!(
        limit(&response(429, &[("retry-after", "999999")]), now),
        Some(Limit::Refused(now + MAX_WAIT))
    );
}

#[test]
fn permission_errors_and_remaining_requests_are_not_limits() {
    let now = 1_800_000_000;
    assert_eq!(limit(&response(403, &[]), now), None);
    let remaining = [
        ("x-ratelimit-remaining", "12"),
        ("x-ratelimit-reset", "1800000600"),
    ];
    assert_eq!(limit(&response(200, &remaining), now), None);
    let exhausted = [
        ("x-ratelimit-remaining", "0"),
        ("x-ratelimit-reset", "1800000600"),
    ];
    assert_eq!(
        limit(&response(200, &exhausted), now),
        Some(Limit::Exhausted(now + 600))
    );
}

#[test]
fn limits_are_found_among_an_errors_causes() {
    let limited = RateLimited {
        host: "forge.example".into(),
        until: 1,
    };
    let error = anyhow::Error::new(limited.clone()).context("GET issues/1");
    assert_eq!(rate_limited(&error), Some(&limited));
    assert_eq!(rate_limited(&anyhow::anyhow!("HTTP 404")), None);
}
