use super::*;
use crate::model::Chat;

/// A chat named `name`, with a session to continue, in the worktree of
/// `workspace` when given.
fn chat(name: &str, workspace: Option<&str>) -> Chat {
    serde_json::from_value(serde_json::json!({
        "id": name, "repository_id": "repo", "name": name, "agent": "claude",
        "started_at": 0, "session": "session",
        "workdir": match workspace {
            Some(workspace) => serde_json::json!({
                "kind": "worktree", "path": format!("/{workspace}"),
                "workspace": {"id": workspace, "name": workspace, "branch": workspace},
            }),
            None => serde_json::json!({"kind": "checkout", "path": "/repo"}),
        },
    }))
    .unwrap()
}

/// The workspace `name`, with the spaces `children` below it.
fn workspace(name: &str, children: Vec<SpaceRef>) -> crate::model::Workspace {
    let mut workspace = crate::model::Workspace::new_record(
        "repo".into(),
        name.into(),
        format!("/work/{name}").into(),
        crate::model::Head::Branch(name.into()),
        crate::state::WorkspaceState::Ready,
    );
    workspace.id = name.into();
    workspace.children = children;
    workspace
}

fn space(id: &str, name: &str, kind: SpaceKind) -> SpaceRef {
    SpaceRef {
        id: id.into(),
        name: name.into(),
        kind,
        branch: None,
    }
}

/// The export check of `workspace`, whose removal takes `taken` first.
fn checked(workspace: crate::model::Workspace, taken: Vec<SpaceRef>) -> Checked {
    let worktree = serde_json::json!({
        "workspace": workspace, "running_commands": 0, "processes": [], "dirty": false,
        "unpushed_commits": 0, "branch": "worker", "matches_default_branch": true,
    });
    let check: SpaceRemovalCheck = serde_json::from_value(serde_json::json!({
        "space": workspace, "worktree": worktree, "children": taken,
    }))
    .unwrap();
    Checked {
        taken: check.children.clone(),
        space: Ending::Workspace(check),
        carried: Vec::new(),
    }
}

#[test]
fn a_space_goes_only_with_the_workspaces_below_it_while_its_chats_go_with_it() {
    let taken = vec![
        space("split", "split", SpaceKind::Workspace),
        space("reading", "reading", SpaceKind::Chat),
    ];
    let check = checked(workspace("worker", Vec::new()), taken);
    let worker = SpaceId::from("worker");

    let error = check.ensure_children_exported(&[&worker]).unwrap_err();
    assert!(error.to_string().contains("(split)"), "{error}");
    check
        .ensure_children_exported(&[&worker, &SpaceId::from("split")])
        .unwrap();
}

#[test]
fn a_child_sharing_its_parents_name_is_not_taken_as_exported() {
    let child = space("child-id", "worker", SpaceKind::Workspace);
    let check = checked(workspace("worker", Vec::new()), vec![child]);
    let worker = SpaceId::from("worker");

    let error = check.ensure_children_exported(&[&worker]).unwrap_err();
    assert!(error.to_string().contains("(worker)"), "{error}");
    check
        .ensure_children_exported(&[&worker, &SpaceId::from("child-id")])
        .unwrap();
}

#[test]
fn a_batch_ends_the_chats_that_go_with_a_space_with_it_and_spaces_below_first() {
    let below = |id, kind| space(id, id, kind);
    let child: Chat = serde_json::from_value(serde_json::json!({
        "workdir": {"kind": "checkout", "path": "/repo"},
        "id": "child", "repository_id": "repo", "name": "child", "agent": "claude",
        "started_at": 0, "session": "session",
        "parent": below("worker", SpaceKind::Workspace),
    }))
    .unwrap();
    let split = workspace("split", Vec::new());
    let worker = workspace(
        "worker",
        vec![
            below("split", SpaceKind::Workspace),
            below("child", SpaceKind::Chat),
        ],
    );
    let tree = Spaces(vec![
        worker.clone().into(),
        split.clone().into(),
        child.clone().into(),
        chat("talk", None).into(),
    ]);
    let batch = [
        worker.into(),
        child.into(),
        split.into(),
        chat("talk", None).into(),
    ];

    let alone = exported_alone(&batch, &tree);

    let ids: Vec<&str> = alone.iter().map(|space| space.id.as_str()).collect();
    assert_eq!(ids, ["split", "talk", "worker"]);
}
