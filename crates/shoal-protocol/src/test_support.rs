//! Values other crates' tests build their spaces from.
use std::path::Path;

use crate::model::{Space, Workspace};

/// A ready workspace `id` of the repository `repository` on the branch
/// `branch`, with its worktree at `path`.
pub fn workspace(id: &str, repository: &str, branch: &str, path: &Path) -> Workspace {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "number": 1,
        "name": branch,
        "repository_id": repository,
        "workdir": {
            "kind": "worktree",
            "workspace": {"id": id, "name": branch, "branch": branch},
            "path": path,
        },
        "kind": "workspace",
        "path": path,
        "branch": branch,
        "state": "ready",
        "checkout": {"state": "created", "base": {"commit": "abc"}, "git_dir": "/admin"},
        "error": null,
        "base_commit": null,
        "base_ref": null,
        "git_dir": null,
    }))
    .unwrap()
}

/// The space `workspace` is.
pub fn space(workspace: &Workspace) -> Space {
    Space::from(workspace.clone())
}
