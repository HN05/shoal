use std::collections::HashSet;

use super::*;
use crate::cli::ui::rows::tests::workspace;
use crate::state::WorkspaceState;

#[test]
fn tree_rows_list_children_below_their_parent() {
    let plain = Palette::stdout(true);
    let mut workspaces = vec![
        workspace("a", "child", "child", WorkspaceState::Ready),
        workspace("a", "grandchild", "grandchild", WorkspaceState::Ready),
        workspace("a", "orphan", "orphan", WorkspaceState::Ready),
        workspace("a", "parent", "parent", WorkspaceState::Ready),
    ];
    let reference = |w: &Workspace| crate::model::SpaceRef {
        id: w.id.clone(),
        name: w.name.clone(),
        kind: crate::model::SpaceKind::Workspace,
        branch: w.branch.clone(),
    };
    workspaces[0].parent = Some(reference(&workspaces[3]));
    workspaces[1].parent = Some(reference(&workspaces[0]));
    // A parent left out of the list leaves its child at the top level.
    workspaces[2].parent = Some(crate::model::SpaceRef {
        id: "unlisted".into(),
        name: "unlisted".into(),
        kind: crate::model::SpaceKind::Workspace,
        branch: Some("unlisted".into()),
    });
    let rows = workspace_tree_rows(&workspaces, &[], &Statuses::default(), plain, None);
    let expected = [
        "○ 1  orphan ",
        "○ 1  parent ",
        "○ 1  └ child ",
        "○ 1    └ grandchild ",
    ];
    assert_eq!(rows.len(), expected.len());
    for (row, start) in rows.iter().zip(expected) {
        assert!(row.starts_with(start), "{row:?}");
    }
}

#[test]
fn a_ci_jobs_workspace_lists_under_the_workspace_its_run_links() {
    let plain = Palette::stdout(true);
    let pull = workspace("a", "pull", "pull", WorkspaceState::Ready);
    let mut job = workspace("a", "ci-rust-4", "unused", WorkspaceState::Ready);
    job.branch = None;
    job.usage = Some(crate::model::WorkspaceUsage {
        cpu_percent: Some(250.0),
        memory_bytes: 3 << 29,
        processes: 4,
    });
    job.job = Some(Box::new(
        serde_json::from_value(serde_json::json!({
            "job": {"handle": "h", "id": 4, "run": 1, "name": "rust", "labels": ["shoal"]},
            "run": {"url": "", "workflow": "ci.yml", "commit": "abc", "trigger": {"pull": {"number": 12}}},
            "space_ids": [pull.id],
        }))
        .unwrap(),
    ));
    let rows = workspace_tree_rows(&[job, pull], &[], &Statuses::default(), plain, None);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].starts_with("○ 1  pull "), "{rows:?}");
    assert!(rows[1].starts_with("○ 1  └ ci-rust-4 "), "{rows:?}");
    assert!(rows[1].contains("250% CPU, 1.5 GiB"), "{rows:?}");
    assert!(
        rows[1]
            .trim_end()
            .ends_with("CI job ci.yml / rust for PR #12"),
        "{rows:?}"
    );
}

#[test]
fn spaces_under_one_parent_list_by_status_then_number() {
    let plain = Palette::stdout(true);
    let numbered = |name: &str, number: u32, running: bool| Workspace {
        number: number.into(),
        running,
        ..workspace("a", name, name, WorkspaceState::Ready)
    };
    let mut child = numbered("child", 2, false);
    let parent = numbered("parent", 5, false);
    child.parent = Some(crate::model::SpaceRef {
        id: parent.id.clone(),
        name: parent.name.clone(),
        kind: crate::model::SpaceKind::Workspace,
        branch: parent.branch.clone(),
    });
    let mut workspaces = vec![
        numbered("idle", 3, false),
        child,
        parent,
        numbered("busy", 7, true),
        numbered("failed", 1, false),
        numbered("busy-first", 4, true),
    ];
    workspaces[4].state = WorkspaceState::Failed;
    let rows = workspace_tree_rows(&workspaces, &[], &Statuses::default(), plain, None);
    let names: Vec<_> = (rows.iter())
        .map(|row| row.split("  ").nth(1).unwrap_or_default().trim())
        .collect();
    assert_eq!(
        names,
        ["busy-first", "busy", "failed", "idle", "parent", "└ child"]
    );
}

fn entry(name: &str, position: Option<usize>, workspace: Option<&SpaceId>) -> QueueEntry {
    serde_json::from_value(serde_json::json!({
        "id": name, "kind": "launch", "repository_id": "a", "name": name,
        "agent": "claude", "issue_title": "Add onboarding",
        "state": if position.is_some() { "queued" } else { "starting" },
        "position": position, "created_at": 0, "opens": workspace,
    }))
    .expect("a queue entry")
}

#[test]
fn a_queued_agent_shows_as_its_workspaces_status_or_its_own_row() {
    let plain = Palette::stdout(true);
    let numbered = |name: &str, number: u32| Workspace {
        number: number.into(),
        ..workspace("a", name, name, WorkspaceState::Ready)
    };
    let mut runs = numbered("runs", 3);
    runs.running = true;
    let workspaces = [numbered("waits", 2), numbered("idle", 1), runs];
    let statuses = Statuses {
        stopped: HashSet::new(),
        queued: vec![
            entry("new", Some(1), None),
            entry("in-waits", Some(2), Some(&workspaces[0].id)),
            entry("starts", None, None),
            entry("in-runs", Some(3), Some(&workspaces[2].id)),
        ],
    };
    let rows = workspace_tree_rows(&workspaces, &[], &statuses, plain, None);
    assert_eq!(
        rows,
        [
            "◐ 3  runs    process running, queued (3)",
            "◌    starts  starting                     Add onboarding",
            "○ 2  waits   queued (2)",
            "○    new     queued (1)                   Add onboarding",
            "○ 1  idle    inactive",
        ]
    );
}
