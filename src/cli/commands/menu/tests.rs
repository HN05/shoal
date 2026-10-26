use super::*;

#[test]
fn demo_rows_round_trip_their_workspace_and_name() {
    let row = Row::Demo {
        workspace: "workspace-id".into(),
        name: "web".into(),
    };
    let Row::Demo { workspace, name } = Row::parse(row.id()).unwrap() else {
        panic!("expected a demo row");
    };
    assert_eq!((workspace.as_str(), name.as_str()), ("workspace-id", "web"));
}

#[test]
fn untracked_rows_keep_paths_with_tabs_newlines_and_spaces() {
    for path in [
        "/repo/plain",
        "/repo/tab\there",
        "/repo/line\nbreak",
        "/repo/trailing ",
    ] {
        let row = Row::Untracked(PathBuf::from(path));
        let id = row.id();
        assert!(!id.contains(['\t', '\n', ' ']), "{id:?}");
        let Row::Untracked(parsed) = Row::parse(id).unwrap() else {
            panic!("not an untracked row");
        };
        assert_eq!(parsed, PathBuf::from(path));
    }
    assert!(Row::parse("untracked:zz".to_owned()).is_err());
}
