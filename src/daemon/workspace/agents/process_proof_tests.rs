use super::*;

#[tokio::test(start_paused = true)]
async fn transient_incomplete_visibility_is_rechecked() {
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let issue = await_process_proof(timing::WORKSPACE_STOP_TIMEOUT, || async {
        (attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0)
            .then(|| "unreadable".to_owned())
    })
    .await;
    assert_eq!(issue, None);
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
}

#[tokio::test(start_paused = true)]
async fn persistent_incomplete_visibility_reports_the_last_issue() {
    let started = tokio::time::Instant::now();
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let issue = await_process_proof(timing::WORKSPACE_STOP_TIMEOUT, || async {
        let attempt = attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(format!("attempt {attempt}"))
    })
    .await;
    assert_eq!(
        tokio::time::Instant::now() - started,
        timing::WORKSPACE_STOP_TIMEOUT
    );
    let attempts = attempts.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(issue, Some(format!("attempt {}", attempts - 1)));
}
