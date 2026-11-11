use super::*;
use crate::forge::updates::UpdateKind;

#[test]
fn a_rate_limited_poll_waits_for_the_limit_but_at_least_the_interval() {
    let limit = |until| {
        anyhow::Error::new(crate::forge::transport::limits::RateLimited {
            host: "forge.example".into(),
            until,
        })
        .context("GET issues/8")
    };
    let (updates, interval) = limited_poll(
        Err(limit(UnixTime::from_seconds(1_600))),
        UnixTime::from_seconds(1_000),
    )
    .unwrap();
    assert!(updates.is_empty());
    assert_eq!(interval, Duration::from_secs(600));
    let (_, interval) = limited_poll(
        Err(limit(UnixTime::from_seconds(1_001))),
        UnixTime::from_seconds(1_000),
    )
    .unwrap();
    assert_eq!(interval, POLL_INTERVAL);
    let error = limited_poll(
        Err(anyhow::anyhow!("HTTP 500")),
        UnixTime::from_seconds(1_000),
    )
    .unwrap_err();
    assert_eq!(format!("{error:#}"), "HTTP 500");
}

#[tokio::test(start_paused = true)]
async fn a_rate_limited_watch_keeps_polling_after_the_limit() {
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let result = wait_for_updates(None, &mut tokio::sync::watch::channel(0).1, || async {
        match attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) {
            0 => Err(anyhow::Error::new(
                crate::forge::transport::limits::RateLimited {
                    host: "forge.example".into(),
                    until: UnixTime::EPOCH,
                },
            )),
            _ => Err(anyhow::anyhow!("HTTP 500")),
        }
    })
    .await;
    assert_eq!(format!("{:#}", result.unwrap_err()), "HTTP 500");
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
}

#[tokio::test(start_paused = true)]
async fn polling_wakes_for_a_single_result_and_timeout_has_no_update() {
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let started = tokio::time::Instant::now();
    let result = wait_for_updates(
        watch_deadline(Some(3600)),
        &mut tokio::sync::watch::channel(0).1,
        || async {
            Ok(
                if attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                    Vec::new()
                } else {
                    vec![Update {
                        url: "pr".into(),
                        kind: UpdateKind::CiCompleted,
                        message: "review passed".into(),
                        delivery: String::new(),
                        check: None,
                        workspace: None,
                    }]
                },
            )
        },
    )
    .await
    .unwrap();
    assert!(!result.timed_out);
    assert_eq!(result.updates.len(), 1);
    assert_eq!(tokio::time::Instant::now() - started, POLL_INTERVAL);
    let started = tokio::time::Instant::now();
    let result = wait_for_updates(
        watch_deadline(Some(75)),
        &mut tokio::sync::watch::channel(0).1,
        || async { Ok(Vec::new()) },
    )
    .await
    .unwrap();
    assert!(result.timed_out && result.updates.is_empty());
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(75)
    );
    let error = wait_for_updates(
        watch_deadline(Some(3600)),
        &mut tokio::sync::watch::channel(0).1,
        || async { anyhow::bail!("lookup failed") },
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "lookup failed");
}

#[tokio::test(start_paused = true)]
async fn queued_background_updates_wake_a_wait_before_its_next_poll() {
    let (queue, mut queued) = tokio::sync::watch::channel(0);
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let started = tokio::time::Instant::now();
    let wait = wait_for_updates(None, &mut queued, || async {
        Ok(
            if attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                queue.send_modify(|count| *count += 1);
                Vec::new()
            } else {
                vec![Update {
                    url: "pr".into(),
                    kind: UpdateKind::Comment,
                    message: "queued".into(),
                    delivery: String::new(),
                    check: None,
                    workspace: None,
                }]
            },
        )
    });
    assert_eq!(wait.await.unwrap().updates[0].message, "queued");
    assert_eq!(tokio::time::Instant::now(), started);
}

#[tokio::test(start_paused = true)]
async fn polling_without_a_timeout_stays_unbounded() {
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let started = tokio::time::Instant::now();
    let result = wait_for_updates(None, &mut tokio::sync::watch::channel(0).1, || async {
        if attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 120 {
            Ok(vec![Update {
                url: "pr".into(),
                kind: UpdateKind::CiCompleted,
                message: "review passed".into(),
                delivery: String::new(),
                check: None,
                workspace: None,
            }])
        } else {
            Ok(Vec::new())
        }
    })
    .await
    .unwrap();
    assert!(!result.timed_out);
    assert_eq!(result.updates.len(), 1);
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(3600)
    );
}
