//! What the daemon measures about workspaces: activity that restarts idle
//! timers and each workspace's process usage.
//! Each is locked only inside an operation, never across an await.
use shoal_ids::space_ids::SpaceId;
use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, PoisonError},
};

use crate::workspace_usage::WorkspaceUsages;

#[derive(Default)]
pub struct Usage {
    /// Per-workspace activity counters folded into cleanup fingerprints.
    activity: Mutex<HashMap<SpaceId, u64>>,
    /// What each workspace's processes used at the latest sample.
    workspaces: Mutex<WorkspaceUsages>,
}

impl Usage {
    /// Count activity in the workspace `id`.
    pub fn record_activity(&self, id: &SpaceId) {
        *lock(&self.activity).entry(id.to_owned()).or_default() += 1;
    }

    pub fn activity(&self, id: &SpaceId) -> u64 {
        lock(&self.activity).get(id).copied().unwrap_or(0)
    }

    /// Forget the activity of a workspace that no longer exists.
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
