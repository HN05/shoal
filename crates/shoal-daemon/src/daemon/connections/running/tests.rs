use std::time::Duration;

use super::*;

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
