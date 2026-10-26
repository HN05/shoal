use super::*;
use serde_json::json;

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
    let now = 1_800_000_000;
    let mut refused = response(&[("retry-after", "30")]);
    refused.status = 429;
    let limited = RateLimited {
        host: "forge.example".into(),
        until: now + 30,
    };
    assert_eq!(
        memory.observe_limit("forge.example", &refused, now),
        Err(limited.clone())
    );
    assert_eq!(memory.check_limit("forge.example", now + 29), Err(limited));
    assert_eq!(memory.check_limit("other.example", now), Ok(()));
    assert_eq!(memory.check_limit("forge.example", now + 30), Ok(()));
    // The last request before the reset succeeds; the next one waits.
    let exhausted = response(&[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "60")]);
    assert_eq!(
        memory.observe_limit("forge.example", &exhausted, now),
        Ok(())
    );
    assert!(memory.check_limit("forge.example", now + 59).is_err());
    assert_eq!(
        memory.observe_limit("forge.example", &response(&[]), now),
        Ok(())
    );
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
