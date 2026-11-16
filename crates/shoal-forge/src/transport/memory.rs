//! What a long-lived caller, such as Shoal's daemon, remembers about one
//! account's forge responses between requests: the last body of each read,
//! with the validators that let the forge answer `304 Not Modified` instead
//! of sending it again, and until when each forge limits its requests.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use serde_json::Value;
use shoal_time::UnixTime;

use crate::transport::{
    limits::{Limit, RateLimited, limit as reported_limit},
    response::{Headed, Response},
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
    /// Until when each host limits requests: all of them, or those that
    /// count against a limit of their own.
    limits: HashMap<(String, Option<&'static str>), UnixTime>,
}

struct Read {
    /// The conditional request headers that stand for `body`.
    conditions: Vec<String>,
    body: Value,
    used: u64,
}

impl Memory {
    /// Refuse a request to `host` that counts against `limit`, besides the
    /// limit every request shares, at `now` while the forge limits either.
    pub(crate) fn check_limit(
        &self,
        host: &str,
        limit: Option<&'static str>,
        now: UnixTime,
    ) -> Result<(), RateLimited> {
        let mut state = self.lock();
        for limit in std::iter::once(None).chain(limit.map(Some)) {
            let key = (host.to_owned(), limit);
            match state.limits.get(&key) {
                Some(&until) if until > now => {
                    return Err(RateLimited {
                        host: host.to_owned(),
                        limit,
                        until,
                    });
                }
                Some(_) => {
                    state.limits.remove(&key);
                }
                None => {}
            }
        }
        Ok(())
    }

    /// Record the limit `response` from `host` reports at `now`: for the
    /// requests that count against `limit`, or for all of them when the
    /// forge pauses the account. Refuse the response when the forge refused
    /// the request.
    pub(crate) fn observe_limit(
        &self,
        host: &str,
        limit: Option<&'static str>,
        response: &impl Headed,
        now: UnixTime,
    ) -> Result<(), RateLimited> {
        let Some(reported) = reported_limit(response, now) else {
            return Ok(());
        };
        let limit = match reported {
            Limit::Paused(_) => None,
            Limit::Refused(_) | Limit::Exhausted(_) => limit,
        };
        (self.lock().limits).insert((host.to_owned(), limit), reported.until());
        match reported {
            Limit::Paused(until) | Limit::Refused(until) => Err(RateLimited {
                host: host.to_owned(),
                limit,
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
mod tests;
