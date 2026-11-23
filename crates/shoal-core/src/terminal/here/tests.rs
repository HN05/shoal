use super::*;

/// Run this test process in tmux pane `%3`, inside zmx session `work`, as
/// a launch that nested the session there marks it when `nested`.
fn in_zmx_in_tmux(nested: bool) {
    // SAFETY: nextest runs each test in a process of its own, whose other
    // threads leave the environment alone.
    unsafe {
        crate::terminal::hosts::forget_inherited_places();
        std::env::set_var("TMUX", "/tmp/tmux-1000/default,4242,0");
        std::env::set_var("TMUX_PANE", "%3");
        std::env::set_var("ZMX_SESSION", "work");
        match nested {
            true => std::env::set_var(env::NESTED_SESSION, "zmx"),
            false => std::env::remove_var(env::NESTED_SESSION),
        }
    }
}

#[test]
fn a_nested_session_runs_in_the_place_it_is_attached_in() {
    in_zmx_in_tmux(true);
    let place = current_origin().unwrap();
    assert_eq!((place.host, place.id()), (HostName::Tmux, "%3"));
    let nested = place.nested.unwrap();
    assert_eq!((nested.host, nested.id()), (HostName::Zmx, "work"));
}

#[test]
fn a_session_nobody_nested_is_the_place_itself() {
    // zmx claims a terminal before tmux, as the user's own session there.
    in_zmx_in_tmux(false);
    let place = current_origin().unwrap();
    assert_eq!((place.host, place.id()), (HostName::Zmx, "work"));
    assert_eq!(place.nested, None);
}

#[test]
fn the_host_of_the_place_a_session_nests_in_serves_the_terminal() {
    let config = crate::config::Config::from_toml("[terminal.tmux]\nenabled = true\n").unwrap();
    let settings = config
        .resolve(&crate::config::repo::ConfigLayers::default())
        .unwrap();
    in_zmx_in_tmux(true);
    assert_eq!(super::super::current(&settings).host.name(), HostName::Tmux);
    in_zmx_in_tmux(false);
    assert_eq!(super::super::current(&settings).host.name(), HostName::Zmx);
}
