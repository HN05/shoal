//! Per-workspace PR gates: they order a workspace's item changes, links,
//! completion and polls against its cleanup, without making other
//! workspaces wait.
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use anyhow::Result;
use tokio::sync::{Mutex, OwnedMutexGuard};

use super::Manager;
use crate::model::Workspace;

/// One gate per workspace ID. An entry lives while someone holds or awaits
/// its gate, so every concurrent caller for a workspace shares one gate, even
/// one for a workspace that is gone.
#[derive(Default)]
pub(crate) struct PrGates(std::sync::Mutex<HashMap<String, Weak<Mutex<()>>>>);

/// The held gates of one or more workspaces.
pub(crate) struct PrGuard {
    _guards: Vec<OwnedMutexGuard<()>>,
}

impl PrGates {
    /// Lock the gates of every workspace in `ids` together. Callers lock
    /// them in ID order and never take another while they hold one, so
    /// callers with overlapping sets cannot deadlock.
    pub(crate) async fn lock<S: AsRef<str>>(&self, ids: impl IntoIterator<Item = S>) -> PrGuard {
        let mut guards = Vec::new();
        for gate in self.gates(ids) {
            guards.push(gate.lock_owned().await);
        }
        PrGuard { _guards: guards }
    }

    /// [`Self::lock`] when nothing holds any of the gates.
    pub(crate) fn try_lock<S: AsRef<str>>(
        &self,
        ids: impl IntoIterator<Item = S>,
    ) -> Option<PrGuard> {
        let guards = self
            .gates(ids)
            .into_iter()
            .map(|gate| gate.try_lock_owned().ok())
            .collect::<Option<_>>()?;
        Some(PrGuard { _guards: guards })
    }

    /// The gates of `ids` in ID order, each once.
    fn gates<S: AsRef<str>>(&self, ids: impl IntoIterator<Item = S>) -> Vec<Arc<Mutex<()>>> {
        let mut ids: Vec<String> = ids.into_iter().map(|id| id.as_ref().to_owned()).collect();
        ids.sort();
        ids.dedup();
        let mut gates = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        gates.retain(|_, gate| gate.strong_count() > 0);
        ids.into_iter()
            .map(|id| {
                let entry = gates.entry(id).or_default();
                entry.upgrade().unwrap_or_else(|| {
                    let gate = Arc::default();
                    *entry = Arc::downgrade(&gate);
                    gate
                })
            })
            .collect()
    }
}

impl Manager {
    /// The workspace `selector` names, read again once its PR gate is held.
    pub(crate) async fn lock_pr_gate(&self, selector: &str) -> Result<(Workspace, PrGuard)> {
        let id = self.workspace(selector).await?.id;
        let guard = self.pr_gates.lock([&id]).await;
        Ok((self.workspace(&id).await?, guard))
    }
}

#[cfg(test)]
mod tests;
