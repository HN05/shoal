//! Workspace activity, which restarts idle timers, and process usage. Each
//! lock is held only inside an operation, never across an await.
use shoal_ids::space_ids::SpaceId;
use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, PoisonError},
};

use crate::workspace_usage::WorkspaceUsages;

#[derive(Default)]
pub struct Usage {
    /// Folded into cleanup fingerprints.
    activity: Mutex<HashMap<SpaceId, u64>>,
    workspaces: Mutex<WorkspaceUsages>,
}

impl Usage {
    pub fn record_activity(&self, id: &SpaceId) {
        *lock(&self.activity).entry(id.to_owned()).or_default() += 1;
    }

    pub fn activity(&self, id: &SpaceId) -> u64 {
        lock(&self.activity).get(id).copied().unwrap_or(0)
    }

    pub fn forget_workspace(&self, id: &SpaceId) {
        lock(&self.activity).remove(id);
    }

    pub fn with_workspaces<R>(&self, f: impl FnOnce(&mut WorkspaceUsages) -> R) -> R {
        f(&mut lock(&self.workspaces))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
