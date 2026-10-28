use super::*;

#[test]
fn list_parses_names_fields_and_labels_after_the_marker() {
    let output = "  name=fix-login\tpid=1\tclients=0\tcreated=1\tcwd=file://host/a\t\
            cmd=shoal internal session\tshoal.agent=claude\tshoal.workspace=w1\n\
            \u{2192} name=other\tpid=2\tclients=1\tcmd=sh -c 'x=y'\n\
            no sessions found in /tmp/zmx-501\n";
    let sessions = parse_list(output);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].0, "fix-login");
    assert_eq!(field(&sessions[0].1, "shoal.workspace"), Some("w1"));
    assert_eq!(field(&sessions[0].1, "shoal.agent"), Some("claude"));
    assert_eq!(sessions[1].0, "other");
    assert_eq!(field(&sessions[1].1, "clients"), Some("1"));
    assert_eq!(field(&sessions[1].1, "cmd"), Some("sh -c 'x=y'"));
}

#[test]
fn state_labels_are_stable_and_whitespace_free() {
    let state = std::path::Path::new("/state with spaces");
    assert_eq!(state_label(state), state_label(state));
    assert!(!state_label(state).contains(char::is_whitespace));
    assert_ne!(
        state_label(state),
        state_label(std::path::Path::new("/other-state"))
    );
}
