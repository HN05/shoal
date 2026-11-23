use super::*;
use crate::sessions::HostName;

#[test]
fn prompts_go_to_the_session_nested_in_the_place() {
    // SAFETY: nextest runs each test in a process of its own, whose other
    // threads leave the environment alone.
    unsafe {
        crate::terminal::hosts::forget_inherited_places();
        std::env::set_var("TMUX", "/tmp/tmux-1000/default,4242,0");
        std::env::set_var("TMUX_PANE", "%3");
        std::env::set_var("ZMX_SESSION", "work");
        std::env::set_var(crate::env::NESTED_SESSION, "zmx");
    }
    let session = PromptSession::current().unwrap();
    assert_eq!(
        (session.place.host, session.place.id()),
        (HostName::Zmx, "work")
    );
}
