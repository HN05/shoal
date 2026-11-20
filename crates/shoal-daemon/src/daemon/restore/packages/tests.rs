use super::copy_warnings;

/// The manifest of a copy of chat `plan`, whose sessions are newer than its
/// files when `earlier_files`.
fn chat_copy(earlier_files: bool) -> crate::transfer::Manifest {
    serde_json::from_value(serde_json::json!({
        "remote_url": "https://forge.test/o/r",
        "record": {
            "workspace_id": "c1",
            "repository_id": "r1",
            "name": "plan",
            "links": {"issues": [], "prs": [], "opened": [], "related": []},
            "cause": "manual",
            "removed_at": 1,
            "expires_at": 2,
            "chat": {"agent": "claude", "mode": "chat"},
        },
        "copy": {"earlier_files": earlier_files},
    }))
    .unwrap()
}

#[test]
fn a_copy_never_continues_the_original_sessions() {
    let refused = copy_warnings(&chat_copy(false), Some(true)).unwrap_err();
    assert!(
        format!("{refused:#}").contains("--keep-sessions"),
        "{refused:#}"
    );
    assert!(copy_warnings(&chat_copy(false), Some(false)).is_ok());
    let warned = copy_warnings(&chat_copy(true), None).unwrap();
    assert_eq!(warned.len(), 1);
    assert!(copy_warnings(&chat_copy(false), None).unwrap().is_empty());
}
