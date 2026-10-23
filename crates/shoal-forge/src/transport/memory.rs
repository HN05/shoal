//! What a long-lived caller, such as Shoal's daemon, remembers about one
//! account's forge responses between requests: the last body of each read,
//! with the validators that let the forge answer `304 Not Modified` instead
//! of sending it again, and until when each forge limits its requests.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use serde_json::Value;

use crate::transport::{
    limits::{Limit, RateLimited, limit},
    response::Response,
};

/// Reads remembered before the least recently used is forgotten.
const REMEMBERED_READS: usize = 512;
/// A longer body is read again instead of remembered.
const MAX_REMEMBERED_BYTES: usize = 1024 * 1024;

/// One account's memory; clones share it.
#[derive(Clone, Default)]
pub struct Memory(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    reads: HashMap<String, Read>,
    /// Counts recalls and reads, ordering them by last use.
    uses: u64,
    /// Unix seconds until which each host limits requests.
    limits: HashMap<String, u64>,
}

struct Read {
    /// The conditional request headers that stand for `body`.
    conditions: Vec<String>,
    body: Value,
    used: u64,
}

impl Memory {
    /// Refuse a request to `host` at `now` while the forge limits them.
    pub(crate) fn check_limit(&self, host: &str, now: u64) -> Result<(), RateLimited> {
        let mut state = self.lock();
        match state.limits.get(host) {
            Some(&until) if until > now => Err(RateLimited {
                host: host.to_owned(),
                until,
            }),
            Some(_) => {
                state.limits.remove(host);
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Record the limit `response` from `host` reports at `now`, and refuse
    /// the response when the forge refused the request.
    pub(crate) fn observe_limit(
        &self,
        host: &str,
        response: &Response,
        now: u64,
    ) -> Result<(), RateLimited> {
        let Some(limit) = limit(response, now) else {
            return Ok(());
        };
        self.lock().limits.insert(host.to_owned(), limit.until());
        match limit {
            Limit::Refused(until) => Err(RateLimited {
                host: host.to_owned(),
                until,
            }),
            Limit::Exhausted(_) => Ok(()),
        }
    }

    /// The conditional headers for the read of `url`, and the body the
    /// forge last sent for it.
    pub(crate) fn recall(&self, url: &str) -> Option<(Vec<String>, Value)> {
        let mut state = self.lock();
        state.uses += 1;
        let used = state.uses;
        let read = state.reads.get_mut(url)?;
        read.used = used;
        Some((read.conditions.clone(), read.body.clone()))
    }

    /// Remember `body`, which a successful read of `url` sent in `response`,
    /// when the forge gave validators for it.
    pub(crate) fn remember(&self, url: &str, response: &Response, body: &Value) {
        let mut state = self.lock();
        let conditions = conditions(response);
        if conditions.is_empty() || response.body.len() > MAX_REMEMBERED_BYTES {
            state.reads.remove(url);
            return;
        }
        state.uses += 1;
        let used = state.uses;
        state.reads.insert(
            url.to_owned(),
            Read {
                conditions,
                body: body.clone(),
                used,
            },
        );
        if state.reads.len() > REMEMBERED_READS
            && let Some(oldest) = (state.reads.iter())
                .min_by_key(|(_, read)| read.used)
                .map(|(url, _)| url.clone())
        {
            state.reads.remove(&oldest);
        }
    }
}

impl Memory {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The conditional request headers for the validators `response` carries.
fn conditions(response: &Response) -> Vec<String> {
    [
        ("etag", "If-None-Match"),
        ("last-modified", "If-Modified-Since"),
    ]
    .into_iter()
    .filter_map(|(validator, condition)| {
        response
            .header(validator)
            .filter(|value| !value.contains(['\r', '\n']))
            .map(|value| format!("{condition}: {value}"))
    })
    .collect()
}

#[cfg(test)]
mod tests {
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
}
