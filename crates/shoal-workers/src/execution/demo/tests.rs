use super::*;

#[test]
fn quick_exits_back_off_and_a_settled_run_restarts_at_once() {
    let mut restarts = Restarts::default();
    let quick = Duration::from_secs(1);
    let delays: Vec<_> = (0..7).map(|_| restarts.after(quick).as_secs()).collect();
    assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);
    assert_eq!(restarts.after(SETTLED), Duration::ZERO);
    assert_eq!(restarts.after(quick), FIRST_DELAY);
}
