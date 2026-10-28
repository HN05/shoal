use super::*;

#[test]
fn durations_show_their_two_largest_units() {
    assert_eq!(elapsed(Duration::from_secs(45)), "45s");
    assert_eq!(elapsed(Duration::from_secs(125)), "2m 5s");
    assert_eq!(elapsed(Duration::from_secs(7_380)), "2h 3m");
}

#[tokio::test(start_paused = true)]
async fn the_summary_lists_the_most_held_method_first_with_its_oldest_request() {
    let running = Arc::new(Running::default());
    let _status = running.start("status".into());
    let _first = running.start("agent_updates".into());
    tokio::time::advance(Duration::from_secs(90)).await;
    let _second = running.start("agent_updates".into());
    assert_eq!(
        running.summary(),
        "2 agent_updates (oldest 1m 30s), 1 status (oldest 1m 30s)"
    );
}

#[tokio::test(start_paused = true)]
async fn a_finished_request_leaves_the_summary() {
    let running = Arc::new(Running::default());
    drop(running.start("status".into()));
    assert_eq!(running.summary(), "");
}

#[tokio::test(start_paused = true)]
async fn a_slow_request_still_returns_its_output() {
    let slow = async {
        tokio::time::sleep(SLOW_REQUEST * 2).await;
        7
    };
    assert_eq!(logged_when_slow("status", slow).await, 7);
}
