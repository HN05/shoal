//! Restart a managed daemon after its installed executable is replaced.
use super::workspace::Manager;
use crate::fsutil::FileIdentity;
use std::path::PathBuf;
use tokio::sync::RwLockWriteGuard;

pub(super) struct Update {
    pub(super) path: PathBuf,
    original: FileIdentity,
}

impl Update {
    /// Record the executable this daemon was started from. The daemon calls
    /// this before any startup work, while the path still names the running
    /// binary, so a replacement during startup is already pending.
    pub(super) fn new(managed: bool) -> Option<Self> {
        if !managed {
            return None;
        }
        let path = crate::service::executable(None).ok()?;
        let original = FileIdentity::of(&path).ok()?;
        Some(Self { path, original })
    }

    pub(super) fn pending(&self) -> bool {
        crate::fsutil::is_executable(&self.path).unwrap_or(false)
            && self.original.replaced_at(&self.path)
    }
}

/// Admission is paused in the server while checking. Background readers cover
/// requests, hooks and mutations; execution connections hold one only while
/// their execution changes or their wrapper cannot reattach. Connected
/// executions that never reattach block restart.
pub(super) fn quiesce(manager: &Manager) -> Option<RwLockWriteGuard<'_, ()>> {
    let guard = manager.gates.try_quiesce()?;
    (!manager.executions.has_unattachable()).then_some(guard)
}

#[cfg(test)]
mod tests;
