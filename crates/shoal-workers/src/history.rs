//! The spaces each command last acted on, so `-` can name the previous one.
use shoal_core::paths::StateFiles;
use shoal_ids::space_ids::SpaceId;
use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    client::connection::CommandPaths,
    fsutil::atomic::{Permissions, ReplaceOptions},
    model::{Space, Spaces},
    paths::Paths,
};
use shoal_protocol::space_names::SpaceSelector;

/// The history `attach -` reads.
pub const ATTACH: &str = "attach";

/// Entries kept per command, enough to pass over the current space and
/// removed ones.
const LIMIT: usize = 8;

/// Space IDs by command name, most recent first.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct History(BTreeMap<String, Vec<SpaceId>>);

impl History {
    /// An unreadable history is empty: it only saves typing a name.
    pub fn load(paths: &Paths) -> Self {
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

    /// The most recent of the space IDs `spaces` that `command` acted on,
    /// leaving out those in `current`.
    pub fn previous(
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

/// The spaces a command here runs in, the one it leaves first: the
/// workspace the current directory is in, then the space this process
/// belongs to.
pub fn current(spaces: &Spaces) -> Result<Vec<SpaceId>> {
    let workspace = crate::workspace_context::of_directory(&spaces.workspaces())?
        .map(|workspace| workspace.id.clone());
    Ok(workspace
        .into_iter()
        .chain(crate::env::own_space())
        .collect())
}

/// Remember that this terminal showed an agent session of the space
/// `selector` names, whose agent runs or is about to, and left it running,
/// so `attach -` returns to it whichever process showed it.
pub async fn record_attach(paths: &CommandPaths, selector: &SpaceSelector) {
    remember(paths, ATTACH, selector, false, |_| true).await;
}

/// [`record_attach`] for `space`, another joined machine's, which this
/// machine does not list.
pub fn record_attach_elsewhere(paths: &CommandPaths, space: &SpaceId) {
    if keeps_history(paths) {
        let _ = History::update(paths, |history| history.record(ATTACH, space));
    }
}

/// Remember that `command` acted on the space `selector` names when
/// `accepts` takes it, and on the current space first when
/// `from_current`. Failing to keep the history never fails the command.
pub async fn remember(
    paths: &CommandPaths,
    command: &str,
    selector: &SpaceSelector,
    from_current: bool,
    accepts: impl Fn(&Space) -> bool,
) {
    if !keeps_history(paths) {
        return;
    }
    let Ok(spaces) = crate::client::spaces::list(paths).await else {
        return;
    };
    let Some(target) = spaces.find(selector).filter(|space| accepts(space)) else {
        return;
    };
    let current = from_current
        .then(|| current(&spaces).ok()?.into_iter().next())
        .flatten();
    let _ = History::update(paths, |history| {
        if let Some(current) = current {
            history.record(command, &current);
        }
        history.record(command, &target.id);
    });
}

/// Whether this caller keeps a history: scoped callers keep none.
pub fn keeps_history(paths: &Paths) -> bool {
    !crate::process::scoped::inherits_scope(&paths.state)
}

#[cfg(test)]
mod tests;
