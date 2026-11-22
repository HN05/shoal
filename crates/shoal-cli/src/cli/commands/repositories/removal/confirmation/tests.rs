use serde_json::json;

use super::*;

/// The preview of `app` with `spaces`.
fn preview(spaces: serde_json::Value) -> RepositoryRemovalPreview {
    serde_json::from_value(json!({
        "repository": {
            "id": "r1", "path": "/shoal/app/.checkout", "source": "https://forge/app",
            "last_used": 0, "name": "app", "workspaces_dir": "/shoal/app",
        },
        "spaces": spaces,
    }))
    .unwrap()
}

#[test]
fn each_space_shows_the_work_its_removal_loses_on_its_machine() {
    let preview = preview(json!([
        {"id": "a", "name": "fix", "kind": "workspace", "number": 3,
         "work": {"dirty": true, "unpushed_commits": 2}},
        {"id": "b", "name": "clean", "kind": "workspace", "number": 4, "work": {}},
        {"id": "c", "name": "lost", "kind": "workspace", "number": 5},
        {"id": "d", "name": "notes", "kind": "chat", "number": 6},
    ]));
    let block = block(&preview, Some("desk"));
    let lines: Vec<&str> = block.lines().collect();
    assert!(lines[0].ends_with("app on desk"), "{block}");
    assert!(
        lines[3].contains("3 fix: uncommitted files, 2 unpushed commits"),
        "{block}"
    );
    assert!(lines[4].trim_end().ends_with("4 clean"), "{block}");
    assert!(lines[5].contains("5 lost: work unknown"), "{block}");
    assert!(lines[6].trim_end().ends_with("6 notes"), "{block}");
}

#[test]
fn a_target_whose_spaces_cannot_be_read_names_its_checkout_and_why() {
    let local: crate::model::Repository = serde_json::from_value(json!({
        "id": "r1", "path": "/shoal/app/.checkout", "source": "https://forge/app",
        "last_used": 0, "name": "app", "workspaces_dir": "/shoal/app",
    }))
    .unwrap();
    let block = unread(&Target::Here(local), &anyhow::anyhow!("admin is propose"));
    assert!(block.contains("/shoal/app/.checkout"), "{block}");
    assert!(block.ends_with("admin is propose"), "{block}");
}
