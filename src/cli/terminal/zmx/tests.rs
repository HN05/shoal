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
fn names_fit_the_socket_path_and_skip_taken_ones() {
    let taken = HashSet::from(["fix".to_owned(), "fix-2".to_owned()]);
    assert_eq!(free_name("fix", &taken), "fix-3");
    assert_eq!(free_name("other", &taken), "other");
    assert_eq!(free_name(&"x".repeat(40), &taken), "x".repeat(MAX_NAME_LEN));
}

#[test]
fn labels_leave_out_agent_names_zmx_cannot_hold() {
    assert_eq!(
        labels(WORKSPACE_LABEL, "w1", "claude"),
        "shoal.workspace=w1 shoal.agent=claude"
    );
    assert_eq!(
        labels(WORKSPACE_LABEL, "w1", "my agent"),
        "shoal.workspace=w1"
    );
    assert_eq!(
        labels(CHAT_LABEL, "r1", "codex"),
        "shoal.chat=r1 shoal.agent=codex"
    );
}

#[test]
fn a_session_hosts_only_the_workspace_it_is_labelled_with() {
    let sessions = parse_list(
        "  name=fix\tpid=1\tshoal.workspace=w1\n  name=other\tpid=2\tshoal.workspace=w2\n  name=bare\tpid=3\n",
    );
    assert!(hosts(&sessions, "fix", "w1"));
    assert!(!hosts(&sessions, "other", "w1"));
    assert!(!hosts(&sessions, "bare", "w1"));
    assert!(!hosts(&sessions, "gone", "w1"));
}
