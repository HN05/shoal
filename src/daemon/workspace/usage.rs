//! What the daemon measures about workspaces and the machine: activity that
//! restarts idle timers, CPU readings and each workspace's process usage.
//! Each is locked only inside an operation, never across an await.
use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, PoisonError},
};

use crate::daemon::{cpu_usage::CpuUsage, workspace_usage::WorkspaceUsages};

#[derive(Default)]
pub(crate) struct Usage {
    /// Per-workspace activity counters folded into cleanup fingerprints.
    activity: Mutex<HashMap<String, u64>>,
    /// Recent CPU readings for dynamic agent admission.
    cpu: Mutex<CpuUsage>,
    /// What each workspace's processes used at the latest sample.
    workspaces: Mutex<WorkspaceUsages>,
}

impl Usage {
    /// Count activity in the workspace `id`.
    pub(crate) fn record_activity(&self, id: &str) {
        *lock(&self.activity).entry(id.to_owned()).or_default() += 1;
    }

    pub(crate) fn activity(&self, id: &str) -> u64 {
        lock(&self.activity).get(id).copied().unwrap_or(0)
    }

    /// Forget the activity of a workspace that no longer exists.
    pub(crate) fn forget_workspace(&self, id: &str) {
        lock(&self.activity).remove(id);
    }

    pub(crate) fn with_cpu<R>(&self, f: impl FnOnce(&mut CpuUsage) -> R) -> R {
        f(&mut lock(&self.cpu))
    }

    pub(crate) fn with_workspaces<R>(&self, f: impl FnOnce(&mut WorkspaceUsages) -> R) -> R {
        f(&mut lock(&self.workspaces))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
