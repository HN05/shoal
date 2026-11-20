use super::*;
use crate::model::{RefPart, SpaceRef};

/// A reference to the workspace or chat `id`.
fn reference(id: &str, kind: SpaceKind) -> SpaceRef {
    SpaceRef {
        id: id.into(),
        name: id.into(),
        part: match kind {
            SpaceKind::Workspace => RefPart::Workspace { branch: None },
            SpaceKind::Chat => RefPart::Chat,
        },
    }
}

/// The chat `id`, below the workspace `parent` when given, whose agent is
/// Claude.
fn chat(id: &str, parent: Option<&str>) -> Space {
    let chat: Chat = serde_json::from_value(serde_json::json!({
        "id": id, "repository_id": "repo", "name": id, "agent": "claude",
        "started_at": 0, "session": "session",
        "parent": parent.map(|parent| reference(parent, SpaceKind::Workspace)),
        "workdir": {"kind": "checkout", "path": "/repo"},
    }))
    .unwrap();
    chat.into()
}

/// The workspace `id`, below the workspace `parent` when given.
fn workspace(id: &str, parent: Option<&str>) -> Space {
    let mut workspace = crate::model::Workspace::new_record(
        "repo".into(),
        id.into(),
        format!("/work/{id}").into(),
        crate::model::Head::Branch(id.into()),
        crate::state::WorkspaceState::Ready,
    );
    workspace.id = id.into();
    workspace.parent = parent.map(|parent| reference(parent, SpaceKind::Workspace));
    workspace.into()
}

fn ids(spaces: &[Space]) -> Vec<&str> {
    spaces.iter().map(|space| space.id.as_str()).collect()
}

/// A tree of spaces: `split` and the chat `talk` below `worker`,
/// `nested` below `split`, and `other` on its own.
fn tree() -> Spaces {
    Spaces(vec![
        workspace("worker", None),
        workspace("split", Some("worker")),
        workspace("nested", Some("split")),
        chat("talk", Some("worker")),
        workspace("other", None),
    ])
}

#[test]
fn a_batch_writes_a_file_for_each_space_no_other_selected_space_carries() {
    let tree = tree();
    let find = |id: &str| tree.iter().find(|space| space.id == id).unwrap().clone();
    let batch = [find("nested"), find("worker"), find("other")];

    let alone = exported_alone(&batch, &tree, &Carrying::Subtree);
    assert_eq!(ids(&alone), ["worker", "other"]);

    // Without the spaces below, only a selected one goes in its parent's
    // file, and one below a space that stays goes in a file of its own.
    let selected = Carrying::Selected(vec!["nested".into(), "split".into()]);
    let batch = [find("worker"), find("split"), find("nested")];
    assert_eq!(ids(&exported_alone(&batch, &tree, &selected)), ["worker"]);
    let selected = Carrying::Selected(vec!["worker".into(), "nested".into()]);
    let batch = [find("worker"), find("nested")];
    assert_eq!(
        ids(&exported_alone(&batch, &tree, &selected)),
        ["worker", "nested"]
    );
}
