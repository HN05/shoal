use super::*;

#[test]
fn durations_read_in_minutes_and_seconds() {
    assert_eq!(duration(std::time::Duration::from_secs(42)), "42s");
    assert_eq!(duration(std::time::Duration::from_secs(192)), "3m 12s");
    assert_eq!(short("0123456789abcdef"), "0123456789ab");
}
