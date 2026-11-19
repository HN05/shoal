//! The spaces each command last acted on, so `-` can name the previous one.
use crate::paths::StateFiles;
use shoal_ids::space_ids::SpaceId;
use std::collections::BTreeMap;

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    cli::{context::Context, ui},
    fsutil::atomic::{Permissions, ReplaceOptions},
    model::{Space, Spaces},
    paths::Paths,
};
use shoal_protocol::space_names::SpaceSelector;

/// Whether `selector` is `-`, which names the previous space.
pub fn is_previous(selector: &SpaceSelector) -> bool {
    *selector == SpaceSelector::Previous
}

/// The history `attach -` reads, which every command that shows a space's
/// agent session here keeps.
pub const ATTACH: &str = "attach";

/// Entries kept per command, enough to pass over the current space and
/// removed ones.
const LIMIT: usize = 8;

/// Space IDs by command name, most recent first.
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

/// The space the command last acted on other than the current workspace
/// and the space this process runs in.
pub async fn previous(ctx: &Context) -> Result<SpaceId> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    previous_among(ctx, &spaces)
}

/// [`previous`] among `spaces`, for a command that returns only to some.
pub fn previous_among(ctx: &Context, spaces: &Spaces) -> Result<SpaceId> {
    let command = command(ctx)?;
    let current = current(spaces)?;
    let listed: Vec<&SpaceId> = spaces.iter().map(|space| &space.id).collect();
    let current: Vec<&SpaceId> = current.iter().collect();
    History::load(&ctx.paths)
        .previous(command, &listed, &current)
        .with_context(|| format!("no previous space for `shoal {command}`"))
}

/// The history the command keeps, outside workspace processes, which keep
/// none.
fn command(ctx: &Context) -> Result<&str> {
    let command = ctx
        .recent
        .as_deref()
        .context("this command does not accept - for a previous space")?;
    ensure!(
        keeps_history(ctx),
        "workspace processes have no previous space"
    );
    Ok(command)
}

/// The spaces a command here runs in, the one it leaves first: the
/// workspace the current directory is in, then the space this process
/// belongs to.
fn current(spaces: &Spaces) -> Result<Vec<SpaceId>> {
    let workspace =
        ui::current_workspace(&spaces.workspaces())?.map(|workspace| workspace.id.clone());
    Ok(workspace
        .into_iter()
        .chain(crate::env::own_space())
        .collect())
}

/// Remember that the command acted on the space `selector` names.
pub async fn record(ctx: &Context, selector: &SpaceSelector) {
    record_where(ctx, selector, |_| true).await;
}

/// [`record`] for a command that acts only on the spaces `accepts` takes,
/// so one it refuses stays out of its history.
pub async fn record_where(
    ctx: &Context,
    selector: &SpaceSelector,
    accepts: impl Fn(&Space) -> bool,
) {
    if let Some(command) = ctx.recent.as_deref() {
        remember(ctx, command, selector, false, accepts).await;
    }
}

/// Remember that the command moved from the current space to the one
/// `selector` names, so `-` returns to the one it left.
pub async fn record_move(ctx: &Context, selector: &SpaceSelector) {
    if let Some(command) = ctx.recent.as_deref() {
        remember(ctx, command, selector, true, |_| true).await;
    }
}

/// Remember that this terminal showed an agent session of the space
/// `selector` names, whose agent runs or is about to, and left it running,
/// so `attach -` returns to it whichever command showed it.
pub async fn record_attach(ctx: &Context, selector: &SpaceSelector) {
    remember(ctx, ATTACH, selector, false, |_| true).await;
}

/// Remember the space `selector` names when `accepts` takes it, and the
/// current space first when `from_current`. Failing to keep the history
/// never fails the command.
async fn remember(
    ctx: &Context,
    command: &str,
    selector: &SpaceSelector,
    from_current: bool,
    accepts: impl Fn(&Space) -> bool,
) {
    if !keeps_history(ctx) {
        return;
    }
    let Ok(spaces) = crate::client::spaces::list(&ctx.paths).await else {
        return;
    };
    let Some(target) = spaces.find(selector).filter(|space| accepts(space)) else {
        return;
    };
    let current = from_current
        .then(|| current(&spaces).ok()?.into_iter().next())
        .flatten();
    let _ = History::update(&ctx.paths, |history| {
        if let Some(current) = current {
            history.record(command, &current);
        }
        history.record(command, &target.id);
    });
}

/// Whether this caller keeps a history: scoped callers keep none.
fn keeps_history(ctx: &Context) -> bool {
    !crate::process::scoped::inherits_scope(&ctx.paths.state)
}

#[cfg(test)]
mod tests;
