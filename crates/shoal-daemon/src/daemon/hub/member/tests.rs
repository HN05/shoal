use super::{Link, UNSEALED_ALERT_EVERY, UnsealedAlert};
use crate::daemon::hub::SHOWN_TEXT;

#[test]
fn a_link_keeps_its_problem_bounded_whatever_the_other_side_sent() {
    let link = Link::default();
    let welcome = format!(
        "expected the hub's welcome, not Welcome {{ name: {:?} }}",
        "x".repeat(1 << 20)
    );
    link.set(None, Some(welcome));
    let problem = link.state.lock().unwrap().problem.clone().unwrap();
    assert!(problem.len() <= SHOWN_TEXT, "{}", problem.len());
}

#[tokio::test(start_paused = true)]
async fn the_user_hears_of_unsealed_refusals_at_most_once_a_while() {
    let alert = UnsealedAlert::default();
    assert!(alert.due());
    assert!(!alert.due());
    tokio::time::advance(UNSEALED_ALERT_EVERY).await;
    assert!(alert.due());
}
