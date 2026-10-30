use super::*;

#[test]
fn the_add_row_adds_and_refuses_what_needs_a_file() {
    assert_eq!(
        resolve_action(FileAction::Print, ADD_ENTRY).unwrap(),
        FileAction::Add
    );
    assert_eq!(
        resolve_action(FileAction::Add, ADD_ENTRY).unwrap(),
        FileAction::Add
    );
    for action in [FileAction::PrintPath, FileAction::Open, FileAction::Delete] {
        assert!(resolve_action(action, ADD_ENTRY).is_err(), "{action:?}");
    }
    assert_eq!(
        resolve_action(FileAction::Delete, "7").unwrap(),
        FileAction::Delete
    );
}

#[test]
fn each_file_is_a_row_by_its_record_before_the_add_row() {
    let file = AttachedFile {
        id: 7,
        workspace: "audit".into(),
        source: crate::daemon::workspace_files::FileSource::Result,
        name: "report.md".into(),
        path: "/repo/.attachments/results/id/key/0/report.md".into(),
        bytes: 960,
        created_at: 0,
        attached_by: None,
        expires_at: None,
    };
    let rows = entries(std::slice::from_ref(&file));
    let ids: Vec<_> = rows.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["7", ADD_ENTRY]);
    assert!(rows[0].1.contains("report.md"), "{}", rows[0].1);
    assert_eq!(rows[1].1, "+ Add file");
}
