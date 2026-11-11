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
    let rows = workspace_tree_rows(&workspaces, &[], &HashSet::new(), plain, None);
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
    let rows = workspace_tree_rows(&[job, pull], &[], &HashSet::new(), plain, None);
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
