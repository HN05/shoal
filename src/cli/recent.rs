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

    /// Apply `change` under a lock, so concurrent commands keep each other's
    /// entries.
    fn update(paths: &Paths, change: impl FnOnce(&mut Self)) -> Result<()> {
        let path = paths.recent_workspaces();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.with_extension("json.lock"))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        let mut history = Self::load(paths);
        change(&mut history);
        history.save(paths)
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
    let current = from_current
        .then(|| ui::current_workspace(&workspaces).ok().flatten())
        .flatten();
    let _ = History::update(&ctx.paths, |history| {
        if let Some(current) = current {
            history.record(command, &current.id);
        }
        history.record(command, &target.id);
    });
}

#[cfg(test)]
mod tests;
