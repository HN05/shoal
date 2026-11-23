use crate::restore::takeovers::*;
use crate::time::UnixTime;
use std::time::Duration;

#[test]
fn a_takeover_names_how_old_the_oldest_change_its_package_lacks_is() {
    let now = UnixTime::now();
    assert_eq!(lacking("laptop", None, now), None);
    let since = now - Duration::from_secs(2 * 60 * 60 + 3 * 60);
    let warning = lacking("laptop", Some(since), now).unwrap();
    assert!(warning.contains("laptop") && warning.contains("2h 3m ago"));
}
