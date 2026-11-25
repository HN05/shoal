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

/// The chat `id` in `mode` at `path`, the checkout of the repository
/// `repository` or else the `root_dir` of a chat without one.
pub fn chat(id: &str, repository: Option<&str>, mode: &str, path: &Path) -> crate::model::Chat {
    let workdir = match repository {
        Some(_) => "checkout",
        None => "root_dir",
    };
    serde_json::from_value(serde_json::json!({
        "id": id,
        "workdir": {"kind": workdir, "path": path},
        "number": 1,
        "name": id,
        "repository_id": repository,
        "mode": mode,
        "kind": "chat",
        "started_at": 0,
    }))
    .unwrap()
}
