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
