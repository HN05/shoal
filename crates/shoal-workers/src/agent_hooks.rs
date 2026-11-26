//! The handlers of the hooks Shoal installs for the agents it launches,
//! which `shoal internal` runs.
pub mod changes;
pub mod delivery;
mod input;
pub mod plan;
pub mod subagents;
pub mod turns;
pub mod worktrees;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::{
    client::connection::CommandPaths,
    model::Workspace,
    workspace_context::{self, NoCurrentWorkspace, ScopeOrder},
};
use shoal_protocol::space_names::SpaceSelector;

/// The space a hook reports for: the one `explicit` names, else the
/// agent's own chat, else its own workspace.
async fn own_space(paths: &CommandPaths, explicit: Option<SpaceSelector>) -> Result<SpaceSelector> {
    if let Some(space) = explicit {
        return Ok(space);
    }
    if let Some(chat) = crate::env::own_chat() {
        return Ok(SpaceSelector::Id(chat));
    }
    own_workspace(paths).await
}

/// The workspace the agent's scope belongs to, else, unscoped, the one the
/// hook runs in. Hooks report for the agent's own workspace wherever its
/// shell has moved, such as into a child workspace.
async fn own_workspace(paths: &CommandPaths) -> Result<SpaceSelector> {
    let workspaces = crate::client::spaces::list(paths).await?.into_workspaces();
    match workspace_context::current(&workspaces, ScopeOrder::BeforeDirectory)? {
        Some(workspace) => Ok(SpaceSelector::id(&workspace.id)),
        None => Err(NoCurrentWorkspace.into()),
    }
}

/// The innermost workspace containing `directory`, which a scoped caller
/// lists among its own and those below it.
fn enclosing<'a>(workspaces: &'a [Workspace], directory: &Path) -> Result<&'a Workspace> {
    Workspace::innermost(workspaces, &canonical(directory))
        .with_context(|| format!("{} is in no Shoal workspace", directory.display()))
}

/// `path` with symlinks resolved, as workspace records hold it, or as given
/// when it does not exist.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// Claude Code and Codex read `additionalContext` for the event named in the
/// hook input; without one, the context is plain text.
fn output(input: &str, context: &str) -> String {
    let event = serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|input| input["hook_event_name"].as_str().map(str::to_owned));
    match event {
        Some(event) => serde_json::json!({
            "hookSpecificOutput": {"hookEventName": event, "additionalContext": context}
        })
        .to_string(),
        None => context.to_owned(),
    }
}

#[cfg(test)]
mod tests;
