use std::time::Duration;

use super::*;

fn args(after: Option<&str>, until: Option<&str>) -> SleepTimerArgs {
    SleepTimerArgs {
        after: after.map(|after| after.parse().unwrap()),
        until: until.map(|until| until.parse().unwrap()),
    }
}

#[test]
fn a_timer_fires_after_its_length_or_at_the_next_clock_time() {
    let now = UnixTime::from_seconds(1_795_000_000);
    let message = ["Check".to_owned(), "CI".to_owned()];
    let after = timer(&args(Some("1h30m"), None), &message, now).unwrap();
    assert_eq!(after.at, now + Duration::from_secs(90 * 60));
    assert_eq!(after.message.as_deref(), Some("Check CI"));
    let until = timer(&args(None, Some("14:30")), &[], now).unwrap();
    let clock: shoal_core::triggers::timer::ClockTime = "14:30".parse().unwrap();
    assert_eq!(until.at, clock.next_local(now));
    assert_eq!(until.message, None);
    assert_eq!(timer(&args(None, None), &[], now), None);
}
