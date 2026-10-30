use std::collections::BTreeMap;

use super::*;

#[test]
fn placement_follows_the_setting_then_whether_the_launch_waits() {
    assert_eq!(Placement::choose(None, true), Placement::Focus);
    assert_eq!(Placement::choose(None, false), Placement::Background);
    assert_eq!(Placement::choose(Some(false), true), Placement::Background);
    assert_eq!(Placement::choose(Some(true), false), Placement::Focus);
}

#[test]
fn a_queued_start_keeps_every_server_variable_set_or_not() {
    let selection: BTreeMap<_, _> =
        server_selection(|name| (name == "HERDR_SESSION").then(|| "work".to_owned())).collect();
    assert_eq!(selection["HERDR_SESSION"], "work");
    // The daemon's socket must not stand in for the session the caller chose.
    assert_eq!(selection["HERDR_SOCKET_PATH"], "");
    assert_eq!(selection["REX_SERVER"], "");
}

#[test]
fn a_start_from_no_terminal_records_no_origin_and_no_server() {
    let env = detached_start_environment();
    assert_eq!(recorded_origin(&env[env::ORIGIN]).unwrap(), None);
    assert_eq!(env["HERDR_SESSION"], "");
    assert_eq!(env["REX_SERVER"], "");
}
