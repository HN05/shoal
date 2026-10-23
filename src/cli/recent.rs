//! The workspaces each command last acted on, so `-` can name the previous one.
use std::collections::BTreeMap;

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    cli::{
        client,
        context::Context,
        ui,
        workspace_context::{ScopeOrder, WorkspaceContext},
    },
    fsutil::{Permissions, ReplaceOptions},
    model::Workspace,
    paths::Paths,
};

/// The workspace argument that names the previous workspace.
pub const PREVIOUS: &str = "-";

/// Entries kept per command, enough to pass over the current workspace and
/// removed ones.
const LIMIT: usize = 8;

/// Workspace IDs by command name, most recent first.
#[derive(Debug, Default, Serialize, Deserialize)]
struct History(BTreeMap<String, Vec<String>>);

impl History {
    /// An unreadable history is empty: it only saves typing a name.
    fn load(paths: &Paths) -> Self {
        std::fs::read(paths.recent_workspaces())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, paths: &Paths) -> Result<()> {
        crate::fsutil::replace_atomically(
            &paths.recent_workspaces(),
            &serde_json::to_vec(self)?,
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: false,
            },
        )?;
        Ok(())
    }

    fn record(&mut self, command: &str, workspace_id: &str) {
        let ids = self.0.entry(command.to_owned()).or_default();
        ids.retain(|id| id != workspace_id);
        ids.insert(0, workspace_id.to_owned());
        ids.truncate(LIMIT);
    }

    /// The most recent of `workspaces` other than `current`.
    fn previous<'a>(
        &self,
        command: &str,
        workspaces: &'a [Workspace],
        current: Option<&Workspace>,
    ) -> Option<&'a Workspace> {
        self.0
            .get(command)?
            .iter()
            .filter(|id| current.is_none_or(|current| current.id != **id))
            .find_map(|id| workspaces.iter().find(|workspace| workspace.id == *id))
    }
}

/// The workspace the command last acted on other than the current one.
pub async fn previous(ctx: &Context) -> Result<String> {
    let command = ctx
        .recent
        .as_deref()
        .context("this command does not accept - for a previous workspace")?;
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes have no previous workspace"
    );
    let workspaces = client::workspaces(&ctx.paths).await?;
    let current = ui::current_workspace(&workspaces)?;
    History::load(&ctx.paths)
        .previous(command, &workspaces, current)
        .map(|workspace| workspace.id.clone())
        .with_context(|| format!("no previous workspace for `shoal {command}`"))
}

/// Remember that the command acted on the workspace `selector` names.
pub async fn record(ctx: &Context, selector: &str) {
    remember(ctx, selector, false).await;
}

/// Remember that the command moved from the current workspace to the one
/// `selector` names, so `-` returns to the one it left.
pub async fn record_move(ctx: &Context, selector: &str) {
    remember(ctx, selector, true).await;
}

/// Scoped callers keep no history, and failing to keep it never fails the
/// command.
async fn remember(ctx: &Context, selector: &str, from_current: bool) {
    let Some(command) = ctx.recent.as_deref() else {
        return;
    };
    if crate::env::inherits_scope(&ctx.paths.state) {
        return;
    }
    let Ok(workspaces) = client::workspaces(&ctx.paths).await else {
        return;
    };
    let Some(target) = WorkspaceContext::from_directory(&workspaces, None).resolve(
        Some(selector),
        false,
        ScopeOrder::AfterDirectory,
    ) else {
        return;
    };
    let mut history = History::load(&ctx.paths);
    if from_current && let Ok(Some(current)) = ui::current_workspace(&workspaces) {
        history.record(command, &current.id);
    }
    history.record(command, &target.id);
    let _ = history.save(&ctx.paths);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(id: &str) -> Workspace {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "repository_id": "repo",
            "name": id,
            "path": format!("/{id}"),
            "branch": id,
            "state": "ready",
        }))
        .unwrap()
    }

    #[test]
    fn previous_skips_the_current_and_missing_workspaces() {
        let workspaces = [workspace("a"), workspace("b")];
        let mut history = History::default();
        for id in ["a", "removed", "b"] {
            history.record("cd", id);
        }
        assert_eq!(history.0["cd"], ["b", "removed", "a"]);
        let previous = |current| history.previous("cd", &workspaces, current).map(|w| &w.id);
        assert_eq!(previous(None).unwrap(), "b");
        assert_eq!(previous(workspaces.get(1)).unwrap(), "a");
        assert!(history.previous("attach", &workspaces, None).is_none());
    }

    #[test]
    fn recording_moves_a_workspace_to_the_front_and_keeps_the_limit() {
        let mut history = History::default();
        for id in 0..LIMIT + 2 {
            history.record("attach", &id.to_string());
        }
        history.record("attach", "5");
        let ids = &history.0["attach"];
        assert_eq!(ids.len(), LIMIT);
        assert_eq!(ids[0], "5");
        assert_eq!(ids.iter().filter(|id| *id == "5").count(), 1);
    }
}
