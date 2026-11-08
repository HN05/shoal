//! The spaces each command last acted on, so `-` can name the previous one.
use crate::client::space_ids::SpaceId;
use std::collections::BTreeMap;

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        ui,
        workspace_context::{ScopeOrder, WorkspaceContext},
    },
    fsutil::atomic::{Permissions, ReplaceOptions},
    model::Chat,
    paths::Paths,
};

/// The workspace argument that names the previous workspace.
pub const PREVIOUS: &str = "-";

/// Whether `selector` is `-`, which names the previous space.
pub fn is_previous(selector: &SpaceSelector) -> bool {
    matches!(selector, SpaceSelector::IdOrName(text) if text == PREVIOUS)
}

/// The history `attach -` reads, which every command that shows a
/// workspace's or a chat's agent session here keeps.
pub const ATTACH: &str = "attach";

/// Entries kept per command, enough to pass over the current space and
/// removed workspaces or ended chats.
const LIMIT: usize = 8;

/// Workspace and chat IDs by command name, most recent first.
#[derive(Debug, Default, Serialize, Deserialize)]
struct History(BTreeMap<String, Vec<SpaceId>>);

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
        crate::fsutil::atomic::replace_atomically(
            &paths.recent_workspaces(),
            &serde_json::to_vec(self)?,
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: false,
            },
        )?;
        Ok(())
    }

    fn record(&mut self, command: &str, space_id: &SpaceId) {
        let ids = self.0.entry(command.to_owned()).or_default();
        ids.retain(|id| id != space_id);
        ids.insert(0, space_id.to_owned());
        ids.truncate(LIMIT);
    }

    /// The most recent of the space IDs `spaces` not in `current`.
    fn previous(
        &self,
        command: &str,
        spaces: &[&SpaceId],
        current: &[&SpaceId],
    ) -> Option<SpaceId> {
        self.0
            .get(command)?
            .iter()
            .filter(|id| !current.contains(id))
            .find(|id| spaces.contains(id))
            .cloned()
    }
}

/// The workspace the command last acted on other than the current one.
pub async fn previous(ctx: &Context) -> Result<SpaceId> {
    let command = command(ctx)?;
    previous_among(ctx, command, &[])
        .await?
        .with_context(|| format!("no previous workspace for `shoal {command}`"))
}

/// The workspace or one of the running `chats` the command last acted on,
/// other than the current workspace and the chat this process runs in.
pub async fn previous_space(ctx: &Context, chats: &[Chat]) -> Result<SpaceId> {
    let command = command(ctx)?;
    previous_among(ctx, command, chats)
        .await?
        .with_context(|| format!("no previous space for `shoal {command}`"))
}

/// The history the command keeps, outside workspace processes, which keep
/// none.
fn command(ctx: &Context) -> Result<&str> {
    let command = ctx
        .recent
        .as_deref()
        .context("this command does not accept - for a previous workspace")?;
    ensure!(
        keeps_history(ctx),
        "workspace processes have no previous workspace"
    );
    Ok(command)
}

/// The most recent of the workspaces and `chats` in `command`'s history
/// other than the current workspace and chat.
async fn previous_among(ctx: &Context, command: &str, chats: &[Chat]) -> Result<Option<SpaceId>> {
    let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
    let current_chat = crate::env::chat();
    let current: Vec<&SpaceId> = ui::current_workspace(&workspaces)?
        .map(|workspace| &workspace.id)
        .into_iter()
        .chain(current_chat.as_ref())
        .collect();
    let spaces: Vec<&SpaceId> = workspaces
        .iter()
        .map(|workspace| &workspace.id)
        .chain(chats.iter().map(|chat| &chat.id))
        .collect();
    Ok(History::load(&ctx.paths).previous(command, &spaces, &current))
}

/// Remember that the command acted on the workspace `selector` names.
pub async fn record(ctx: &Context, selector: &SpaceSelector) {
    if let Some(command) = ctx.recent.as_deref() {
        remember(ctx, command, selector, false).await;
    }
}

/// Remember that the command moved from the current workspace to the one
/// `selector` names, so `-` returns to the one it left.
pub async fn record_move(ctx: &Context, selector: &SpaceSelector) {
    if let Some(command) = ctx.recent.as_deref() {
        remember(ctx, command, selector, true).await;
    }
}

/// Remember that this terminal showed an agent session of the workspace
/// `selector` names and left it running, so `attach -` returns to it
/// whichever command showed it.
pub async fn record_attach(ctx: &Context, selector: &SpaceSelector) {
    remember(ctx, ATTACH, selector, false).await;
}

/// Remember that this terminal showed the agent session of the running chat
/// `selector` names or identifies and left it running, as [`record_attach`]
/// does for a workspace. Failing to keep it never fails the command.
pub async fn record_chat_attach(ctx: &Context, selector: &SpaceSelector) {
    if !keeps_history(ctx) {
        return;
    }
    let Ok(chats) = crate::client::chats::list(&ctx.paths).await else {
        return;
    };
    let named = |chat: &&Chat| selector.selects(&chat.id, &chat.name, chat.number);
    if let Some(chat) = chats.iter().find(named) {
        let _ = History::update(&ctx.paths, |history| history.record(ATTACH, &chat.id));
    }
}

/// Failing to keep the history never fails the command.
async fn remember(ctx: &Context, command: &str, selector: &SpaceSelector, from_current: bool) {
    if !keeps_history(ctx) {
        return;
    }
    let Ok(workspaces) = crate::client::workspaces::list(&ctx.paths).await else {
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

/// Whether this caller keeps a history: scoped callers keep none.
fn keeps_history(ctx: &Context) -> bool {
    !crate::env::inherits_scope(&ctx.paths.state)
}

#[cfg(test)]
mod tests;
