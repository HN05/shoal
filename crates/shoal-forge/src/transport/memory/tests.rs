use super::*;
use serde_json::json;
use std::time::Duration;

fn response(headers: &[(&str, &str)]) -> Response {
    Response {
        status: 200,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body: "{}".into(),
    }
}

#[test]
fn reads_are_recalled_by_their_validators_until_a_response_has_none() {
    let memory = Memory::default();
    assert_eq!(memory.recall("a"), None);
    let validated = response(&[
        ("etag", "W/\"1\""),
        ("last-modified", "Thu, 08 Oct 2026 22:20:01 GMT"),
    ]);
    memory.remember("a", &validated, &json!({"n": 1}));
    assert_eq!(
        memory.recall("a"),
        Some((
            vec![
                "If-None-Match: W/\"1\"".to_owned(),
                "If-Modified-Since: Thu, 08 Oct 2026 22:20:01 GMT".to_owned()
            ],
            json!({"n": 1})
        ))
    );
    memory.remember("a", &response(&[]), &json!({"n": 2}));
    assert_eq!(memory.recall("a"), None);
}

#[test]
fn requests_wait_until_a_refusal_or_exhausted_limit_resets() {
    let memory = Memory::default();
    let now = UnixTime::from_seconds(1_800_000_000);
    let seconds = Duration::from_secs;
    let mut refused = response(&[("retry-after", "30")]);
    refused.status = 429;
    let limited = RateLimited {
        host: "forge.example".into(),
        limit: None,
        until: now + seconds(30),
    };
    assert_eq!(
        memory.observe_limit("forge.example", None, &refused, now),
        Err(limited.clone())
    );
    assert_eq!(
        memory.check_limit("forge.example", None, now + seconds(29)),
        Err(limited)
    );
    assert_eq!(memory.check_limit("other.example", None, now), Ok(()));
    assert_eq!(
        memory.check_limit("forge.example", None, now + seconds(30)),
        Ok(())
    );
    // The last request before the reset succeeds; the next one waits.
    let exhausted = response(&[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "60")]);
    assert_eq!(
        memory.observe_limit("forge.example", None, &exhausted, now),
        Ok(())
    );
    assert!(
        memory
            .check_limit("forge.example", None, now + seconds(59))
            .is_err()
    );
    assert_eq!(
        memory.observe_limit("forge.example", None, &response(&[]), now),
        Ok(())
    );
}

#[test]
fn a_limit_of_its_own_holds_back_only_the_requests_it_counts() {
    let memory = Memory::default();
    let now = UnixTime::from_seconds(1_800_000_000);
    let exhausted = response(&[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "60")]);
    assert_eq!(
        memory.observe_limit("forge.example", Some("search"), &exhausted, now),
        Ok(())
    );
    assert_eq!(memory.check_limit("forge.example", None, now), Ok(()));
    assert_eq!(
        memory.check_limit("forge.example", Some("other"), now),
        Ok(())
    );
    let refused = memory.check_limit("forge.example", Some("search"), now);
    assert_eq!(
        refused.map_err(|limited| limited.limit),
        Err(Some("search"))
    );
    // The limit every request shares holds back those with one of their own.
    memory
        .observe_limit("forge.example", None, &exhausted, now)
        .unwrap();
    let refused = memory.check_limit("forge.example", Some("other"), now);
    assert_eq!(refused.map_err(|limited| limited.limit), Err(None));
}

#[test]
fn a_pause_refusing_a_request_with_a_limit_of_its_own_holds_back_every_request() {
    let memory = Memory::default();
    let now = UnixTime::from_seconds(1_800_000_000);
    let mut paused = response(&[("retry-after", "30")]);
    paused.status = 403;
    let refused = memory.observe_limit("forge.example", Some("search"), &paused, now);
    assert_eq!(refused.map_err(|limited| limited.limit), Err(None));
    let held = memory.check_limit("forge.example", None, now);
    assert_eq!(held.map_err(|limited| limited.limit), Err(None));
}

#[test]
fn the_least_recently_used_read_is_forgotten_first() {
    let memory = Memory::default();
    let validated = response(&[("etag", "\"1\"")]);
    for n in 0..REMEMBERED_READS {
        memory.remember(&n.to_string(), &validated, &json!(n));
    }
    memory.recall("0");
    memory.remember("new", &validated, &json!("new"));
    assert!(memory.recall("0").is_some());
    assert!(memory.recall("1").is_none());
    assert!(memory.recall("new").is_some());
}
