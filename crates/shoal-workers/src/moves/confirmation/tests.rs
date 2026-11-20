use super::super::checks::carried_spaces;
use super::*;
use crate::model::{Chat, RefPart, SpaceKind, SpaceRef, Spaces};
use shoal_core::transfer::Carrying;
use shoal_protocol::removal::SpaceRemovalCheck;

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

/// A hold named `name`.
fn hold(name: &str) -> SpaceHold {
    serde_json::from_value(serde_json::json!({
        "space_id": "held", "name": name, "reason": null, "created_at": 0,
    }))
    .unwrap()
}

/// The export check of the workspace `space`, which carries `carried`.
fn checked(space: Space, carried: Vec<Space>) -> Checked {
    let workspace = space.to_workspace().unwrap();
    let worktree = serde_json::json!({
        "workspace": workspace, "running_commands": 0, "processes": [], "dirty": false,
        "unpushed_commits": 0, "branch": "worker", "matches_default_branch": true,
    });
    let check: SpaceRemovalCheck = serde_json::from_value(serde_json::json!({
        "space": workspace, "worktree": worktree, "children": [],
    }))
    .unwrap();
    Checked {
        space: Ending::Workspace(check),
        carried,
    }
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
fn the_confirmation_lists_every_space_that_ends_with_the_named_one() {
    let tree = tree();
    let carried = carried_spaces(&tree, &"worker".into(), &Carrying::Subtree);
    let check = checked(workspace("worker", None), carried);

    let action = ending_action("Export worker".into(), &[&check]).unwrap();

    let listed: Vec<&str> = action.lines().skip(2).collect();
    assert_eq!(
        listed,
        [
            "  split (workspace)",
            "  nested (workspace)",
            "  talk (chat, claude)"
        ]
    );
}

#[test]
fn a_lone_space_asks_only_for_its_holds() {
    assert!(ending_action("Export".into(), &[&checked(workspace("w", None), vec![])]).is_none());

    let mut held = workspace("held", None);
    held.holds = vec![hold("review")];
    let action = ending_action("Export".into(), &[&checked(held, vec![])]).unwrap();
    assert!(action.ends_with("\n  review"), "{action}");
}
