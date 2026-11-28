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

/// The repository `id` registered at its checkout `path`, its workspaces in
/// the directory above.
pub fn repository(id: &str, path: &Path) -> crate::model::Repository {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "path": path,
        "source": path,
        "last_used": 0,
        "workspaces_dir": path.parent().unwrap(),
    }))
    .unwrap()
}

/// The running execution `id` of the space `space`, which registered nothing
/// of its processes or agent.
pub fn execution(id: &str, space: &str) -> crate::model::Execution {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "space_id": space,
        "state": "running",
        "wrapper": null,
        "child": null,
        "group_id": null,
    }))
    .unwrap()
}

/// The event `event` of the workspace `id`, named `name`, with its worktree
/// at `path` and without a repository, branch, cause or error.
pub fn workspace_event(
    id: &str,
    name: &str,
    path: &Path,
    event: crate::events::SpaceEvent,
) -> crate::events::EventDetails {
    crate::events::EventDetails {
        space_id: id.into(),
        space: crate::events::EventSpace::Workspace {
            path: path.to_owned(),
            branch: None,
        },
        repository_id: None,
        name: name.into(),
        cause: None,
        error: None,
        event,
    }
}
