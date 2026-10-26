use super::*;

#[test]
fn statuses_name_the_failure() {
    for (status, failure) in [
        (200, None),
        (302, None),
        (429, Some(Failure::RateLimited)),
        (503, Some(Failure::Overloaded)),
        (529, Some(Failure::Overloaded)),
        (500, Some(Failure::ServerError)),
        (504, Some(Failure::ServerError)),
        (401, Some(Failure::Other)),
        (400, Some(Failure::Other)),
    ] {
        let status = StatusCode::from_u16(status).unwrap();
        assert_eq!(Failure::of_status(status), failure, "{status}");
    }
}

#[test]
fn waits_double_from_a_second_to_half_a_minute() {
    let waits: Vec<_> = (1..=7)
        .map(|attempt| POLICY.backoff(attempt).as_secs())
        .collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
    assert_eq!(POLICY.backoff(u32::MAX), POLICY.max_wait);
}

#[tokio::test(start_paused = true)]
async fn an_outage_is_retried_for_its_budget_until_a_request_succeeds() {
    let outage = Outage::new(POLICY, Arc::new(crate::clock::Tokio::new()));
    assert_eq!(outage.next_wait(1, None), Some(POLICY.first_wait));
    // The upstream's own wait wins, but not past the budget.
    let asked = Some(Duration::from_secs(90));
    assert_eq!(outage.next_wait(2, asked), asked);
    assert_eq!(outage.remaining(), Some(POLICY.budget));
    tokio::time::advance(POLICY.budget - Duration::from_secs(60)).await;
    assert_eq!(outage.remaining(), Some(Duration::from_secs(60)));
    assert_eq!(outage.next_wait(3, None), Some(Duration::from_secs(4)));
    // A wait past the budget gives up, and later requests too.
    assert_eq!(outage.next_wait(4, asked), None);
    assert_eq!(outage.next_wait(1, None), None);
    assert_eq!(outage.remaining(), None);
    outage.ended();
    assert_eq!(outage.remaining(), None);
    assert_eq!(outage.next_wait(1, None), Some(POLICY.first_wait));
}

#[test]
fn retry_after_reads_seconds() {
    let mut headers = HeaderMap::new();
    assert_eq!(retry_after(&headers), None);
    headers.insert("retry-after", "30".parse().unwrap());
    assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));
    headers.insert(
        "retry-after",
        "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
    );
    assert_eq!(retry_after(&headers), None);
}

/// A clock that reads earlier each time, as a request that read the
/// time before another recorded the outage start would see it.
struct Backwards(Mutex<Duration>);

impl crate::clock::Clock for Backwards {
    fn now(&self) -> Duration {
        let mut now = self.0.lock().unwrap();
        *now = now.saturating_sub(Duration::from_secs(1));
        *now
    }

    fn sleep(&self, _: Duration) -> crate::clock::Sleep {
        Box::pin(std::future::pending())
    }
}

#[test]
fn a_failure_read_before_the_outage_started_does_not_break_it() {
    let outage = Outage::new(
        POLICY,
        Arc::new(Backwards(Mutex::new(Duration::from_secs(60)))),
    );
    assert_eq!(outage.next_wait(1, None), Some(POLICY.first_wait));
    // Earlier than the recorded start: the outage keeps its budget.
    assert_eq!(outage.next_wait(2, None), Some(POLICY.first_wait * 2));
    assert_eq!(outage.remaining(), Some(POLICY.budget));
}
