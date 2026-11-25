use anyhow::Result;

use super::*;
use crate::test_support::rows::{check_columns, mapped_rows};

#[test]
fn repository_and_workspace_mappers_use_named_columns() -> Result<()> {
    let db = mapped_rows()?;
    check_columns(
        &db,
        "repositories",
        REPOSITORY_COLUMNS,
        repository,
        serde_json::json!({
            "id": "repo", "path": "/checkout", "source": "origin", "last_used": 42,
            "name": "project", "workspaces_dir": "/workspaces"
        }),
    )?;
    check_columns(
        &db,
        "spaces JOIN workspaces USING(id)",
        WORKSPACE_COLUMNS,
        workspace,
        serde_json::json!({
            "id": "workspace", "kind": "workspace", "number": 1, "repository_id": "repo",
            "name": "worker", "path": "/work",
            "workdir": {"kind": "worktree", "path": "/work",
                        "workspace": {"id": "workspace", "name": "worker", "branch": "feature"}},
            "branch": "feature", "state": "failed", "error": "setup error",
            "checkout": {"state": "created",
                         "base": {"commit": "abc", "reference": "refs/heads/main"},
                         "git_dir": "/git/worktrees/worker"},
            "base_workspace": null, "parent": null, "links": {"issues": [], "prs": [], "opened": [], "related": []}, "running": false, "done": false,
            "setup_skipped": false, "disposable": false
        }),
    )
}
