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
fn session_cleanup_is_scoped_to_its_state_directory() {
    let state = std::path::Path::new("/state with spaces");
    let other = std::path::Path::new("/other-state");
    let owned = parse_list(&format!("name=ours\t{}\n", state_label(state)));
    let foreign = parse_list(&format!("name=theirs\t{}\n", state_label(other)));
    assert!(!state_label(state).contains(char::is_whitespace));
    for (_, fields) in owned {
        assert!(in_state(&fields, state, false));
    }
    for (_, fields) in foreign {
        assert!(!in_state(&fields, state, true));
    }
    assert!(!in_state(&Vec::new(), state, false));
    assert!(in_state(&Vec::new(), state, true));
}

#[test]
fn state_labels_are_stable_and_whitespace_free() {
    let state = std::path::Path::new("/state with spaces");
    assert!(!state_label(state).contains(char::is_whitespace));
    assert_ne!(
        state_label(state),
        state_label(std::path::Path::new("/other-state"))
    );
}

#[test]
fn closing_checks_ownership_even_when_a_name_is_reused() {
    let listed = parse_list("name=reused\tshoal.workspace=w1\tshoal.demo=new-launch\n");
    for (_, fields) in listed {
        assert!(owned_by(
            &fields,
            &super::super::Owner::Workspace("w1".into())
        ));
        assert!(!owned_by(
            &fields,
            &super::super::Owner::Workspace("w2".into())
        ));
        assert!(owned_by(
            &fields,
            &super::super::Owner::Demo("new-launch".into())
        ));
        assert!(!owned_by(
            &fields,
            &super::super::Owner::Demo("old-launch".into())
        ));
    }
}
