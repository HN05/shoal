//! Restart a managed daemon after its installed executable is replaced.
use super::workspace::Manager;
use crate::daemon::log;
use anyhow::{Result, ensure};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::SystemTime,
};
use tokio::sync::RwLockWriteGuard;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutableIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
    size: u64,
    modified: SystemTime,
}

fn identity(path: &Path) -> Result<ExecutableIdentity> {
    let path = fs::canonicalize(path)?;
    let metadata = fs::metadata(&path)?;
    Ok(ExecutableIdentity {
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        modified: metadata.modified()?,
    })
}

fn replaced(original: &ExecutableIdentity, path: &Path) -> bool {
    identity(path).is_ok_and(|current| current != *original)
}

pub(super) struct Update {
    pub(super) path: PathBuf,
    original: ExecutableIdentity,
    known_protocol: Option<(ExecutableIdentity, Option<u32>)>,
}

impl Update {
    pub(super) fn new(managed: bool) -> Option<Self> {
        if !managed {
            return None;
        }
        let path = crate::service::executable(None).ok()?;
        let original = identity(&path).ok()?;
        Some(Self {
            path,
            original,
            known_protocol: None,
        })
    }

    pub(super) fn pending(&self) -> bool {
        crate::fsutil::is_executable(&self.path).unwrap_or(false)
            && replaced(&self.original, &self.path)
    }

    /// An older follower cannot reconnect to an incompatible wire protocol.
    /// Cache the candidate's metadata until its executable changes again.
    pub(super) async fn supports_followers(&mut self, clients: usize) -> Result<bool> {
        if clients == 0 {
            return Ok(true);
        }
        let candidate = identity(&self.path)?;
        if self
            .known_protocol
            .as_ref()
            .is_none_or(|(known, _)| *known != candidate)
        {
            let protocol = match installed_protocol(&self.path).await {
                Ok(protocol) => Some(protocol),
                Err(error) => {
                    log!("daemon update waits for clients: {error:#}");
                    None
                }
            };
            self.known_protocol = Some((candidate, protocol));
        }
        Ok(self
            .known_protocol
            .as_ref()
            .is_some_and(|(_, protocol)| *protocol == Some(crate::protocol::VERSION)))
    }
}

async fn installed_protocol(path: &Path) -> Result<u32> {
    #[derive(serde::Deserialize)]
    struct BuildInfo {
        protocol: u32,
    }
    let mut command = tokio::process::Command::new(path);
    command.arg("__build-info");
    let output = crate::subprocess::Run::new(command)
        .timeout(std::time::Duration::from_secs(15))
        .output()
        .await?;
    let info: BuildInfo = serde_json::from_str(&output)?;
    ensure!(info.protocol > 0, "invalid installed daemon protocol");
    Ok(info.protocol)
}

/// Admission is paused in the server while checking. Existing connections
/// cover requests and connected wrappers; background readers cover hooks and
/// mutations. Unknown executions also block restart until reconciliation.
pub(super) async fn quiesce(manager: &Manager) -> Result<Option<RwLockWriteGuard<'_, ()>>> {
    let Ok(guard) = manager.background_operations.try_write() else {
        return Ok(None);
    };
    Ok((!manager.has_executions().await?).then_some(guard))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn replacement_detects_atomic_package_swap() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shoal");
        let replacement = directory.path().join("shoal.new");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let original = identity(&path).unwrap();
        fs::write(&replacement, "new binary").unwrap();
        fs::rename(replacement, &path).unwrap();
        assert!(replaced(&original, &path));
    }
    #[test]
    fn unchanged_executable_is_not_restarted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shoal");
        fs::write(&path, "same").unwrap();
        let original = identity(&path).unwrap();
        assert!(!replaced(&original, &path));
    }

    #[tokio::test]
    async fn restart_waits_for_background_work_and_execution_records() {
        let (_root, manager) = crate::test_support::manager().await;
        assert!(quiesce(&manager).await.unwrap().is_some());
        let operation = manager.background_operations.read().await;
        assert!(quiesce(&manager).await.unwrap().is_none());
        drop(operation);
        let quiet = quiesce(&manager).await.unwrap().unwrap();
        assert!(manager.background_operations.try_read().is_err());
        drop(quiet);
        manager.store.run(|db| {
            db.execute_batch(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/test','/test',0);
                 INSERT INTO workspaces(id,repository_id,name,path,branch,state)
                    VALUES ('workspace','repo','work','/test/work','work','ready');
                 INSERT INTO executions(id,workspace_id,state) VALUES ('execution','workspace','running');"
            )?;
            Ok(())
        }).await.unwrap();
        assert!(quiesce(&manager).await.unwrap().is_none());
        manager
            .store
            .run(|db| {
                db.execute("UPDATE executions SET state='unknown'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(quiesce(&manager).await.unwrap().is_none());
        manager
            .store
            .run(|db| {
                db.execute("DELETE FROM executions", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(quiesce(&manager).await.unwrap().is_some());
    }
}
