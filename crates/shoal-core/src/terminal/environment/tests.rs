use std::collections::BTreeMap;

use super::*;

#[test]
fn a_queued_start_keeps_every_server_variable_set_or_not() {
    let selection: BTreeMap<_, _> =
        server_selection(|name| (name == "HERDR_SESSION").then(|| "work".to_owned())).collect();
    assert_eq!(selection["HERDR_SESSION"], "work");
    // The daemon's socket must not stand in for the session the caller chose.
    assert_eq!(selection["HERDR_SOCKET_PATH"], "");
    assert_eq!(selection["REX_SERVER"], "");
    assert_eq!(selection["TMUX"], "");
    assert_eq!(selection["TMUX_TMPDIR"], "");
    assert_eq!(selection["ZELLIJ_SOCKET_DIR"], "");
}
