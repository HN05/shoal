use std::cell::{Cell, RefCell};

use anyhow::anyhow;
use tokio::time::Instant;

use super::*;

/// Send with `resend` through scripted results, the last repeating,
/// returning the result, the attempts made and the time waited.
async fn script(
    resend: Resend,
    results: Vec<fn() -> Result<Value>>,
) -> (Result<Value>, usize, Duration) {
    let attempts = Cell::new(0);
    let results = RefCell::new(results);
    let start = Instant::now();
    let result = resending(resend, || {
        attempts.set(attempts.get() + 1);
        let results = results.borrow();
        let next = results[(attempts.get() - 1).min(results.len() - 1)];
        async move { next() }
    })
    .await;
    (result, attempts.get(), start.elapsed())
}

fn unsent() -> Result<Value> {
    Err(anyhow!(Transient::Unsent("could not connect".into())).context("POST x"))
}

fn interrupted() -> Result<Value> {
    Err(Transient::Interrupted("timed out".into()).into())
}

fn busy() -> Result<Value> {
    Err(Transient::Busy("HTTP 503: busy".into()).into())
}

fn refused() -> Result<Value> {
    Err(anyhow!("HTTP 422: invalid"))
}

fn sent() -> Result<Value> {
    Ok(Value::Bool(true))
}

#[tokio::test(start_paused = true)]
async fn passing_failures_are_sent_again_after_longer_waits() {
    let (result, attempts, waited) = script(Resend::Unreceived, vec![unsent, busy, sent]).await;
    assert_eq!(result.unwrap(), Value::Bool(true));
    assert_eq!((attempts, waited), (3, Duration::from_secs(3)));

    let (result, attempts, waited) = script(Resend::Always, vec![interrupted, sent]).await;
    assert!(result.is_ok());
    assert_eq!((attempts, waited), (2, Duration::from_secs(1)));
}

#[tokio::test(start_paused = true)]
async fn other_failures_and_requests_are_sent_once() {
    for (resend, failure) in [
        (Resend::Never, unsent as fn() -> Result<Value>),
        (Resend::Never, busy),
        (Resend::Unreceived, interrupted),
        (Resend::Always, refused),
    ] {
        let (result, attempts, waited) = script(resend, vec![failure, sent]).await;
        assert!(result.is_err(), "{resend:?}");
        assert_eq!((attempts, waited), (1, Duration::ZERO), "{resend:?}");
    }
}

#[test]
fn only_statuses_that_refuse_for_now_are_busy() {
    let kind = |status| match Transient::from_status(status, String::new()) {
        Some(Transient::Busy(_)) => "busy",
        Some(Transient::Interrupted(_)) => "interrupted",
        Some(Transient::Unsent(_)) => "unsent",
        None => "permanent",
    };
    assert_eq!(kind(429), "busy");
    assert_eq!(kind(503), "busy");
    assert_eq!(kind(502), "interrupted");
    assert_eq!(kind(504), "interrupted");
    assert_eq!(kind(422), "permanent");
}

#[tokio::test(start_paused = true)]
async fn the_last_failure_is_returned_once_attempts_run_out() {
    let (result, attempts, waited) = script(Resend::Always, vec![busy]).await;
    assert_eq!(format!("{:#}", result.unwrap_err()), "HTTP 503: busy");
    assert_eq!((attempts, waited), (4, Duration::from_secs(7)));
}
