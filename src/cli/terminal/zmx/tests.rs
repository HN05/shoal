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
        labels(Owner::Workspace("w1"), "claude"),
        "shoal.workspace=w1 shoal.agent=claude"
    );
    assert_eq!(
        labels(Owner::Workspace("w1"), "my agent"),
        "shoal.workspace=w1"
    );
    assert_eq!(
        labels(Owner::Chat("c1"), "codex"),
        "shoal.chat=c1 shoal.agent=codex"
    );
}

#[test]
fn a_watch_follows_only_the_session_it_began_with() {
    let sessions = parse_list(
        "  name=fix\tpid=7\tcreated=100\tshoal.workspace=w1\n\
         \x20 name=other\tpid=8\tcreated=100\tshoal.workspace=w2\n\
         \x20 name=bare\tpid=9\tcreated=100\n",
    );
    let watched = |id: &str, instance: Option<&str>| Watched {
        id: id.into(),
        workspace: "w1".into(),
        instance: instance.map(str::to_owned),
    };
    assert_eq!(instance(&sessions[0].1), "7:100");
    assert!(hosts(&sessions, &watched("fix", Some("7:100"))));
    assert!(hosts(&sessions, &watched("fix", None)));
    // A later session of the same workspace under the same name.
    assert!(!hosts(&sessions, &watched("fix", Some("6:90"))));
    // Another workspace's session, an unlabelled one, or none at all.
    assert!(!hosts(&sessions, &watched("other", None)));
    assert!(!hosts(&sessions, &watched("bare", None)));
    assert!(!hosts(&sessions, &watched("gone", None)));
}
