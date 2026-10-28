use super::*;
use crate::{
    removal::{InspectionPolicy, UnsavedWork},
    sessions::{Host, Session},
    test_support::{manager, repository},
};
use anyhow::Context as _;
use futures_util::future::BoxFuture;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
struct FakeHost {
    remaining: Mutex<Vec<(String, String)>>,
    closed: Mutex<Vec<String>>,
    fail: AtomicBool,
}

impl FakeHost {
    fn add(&self, name: &str, workspace: &str) -> Result<()> {
        self.remaining
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .push((name.into(), workspace.into()));
        Ok(())
    }

    fn closed(&self) -> Result<Vec<String>> {
        Ok(self
            .closed
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .clone())
    }
}

impl Host for FakeHost {
    fn name(&self) -> &'static str {
        "zmx"
    }
    fn sessions<'a>(&'a self, _: &'a Path, _: bool) -> BoxFuture<'a, Result<Vec<Session>>> {
        Box::pin(async {
            Ok(self
                .remaining
                .lock()
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .iter()
                .map(|(name, workspace)| Session {
                    name: name.clone(),
                    workspace: workspace.clone(),
                })
                .collect())
        })
    }

    fn close<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            anyhow::ensure!(
                !(self.fail.load(Ordering::Relaxed) && name == "bad"),
                "close failed"
            );
            self.closed
                .lock()
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .push(name.into());
            self.remaining
                .lock()
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .retain(|(session, _)| session != name);
            Ok(())
        })
    }
}

async fn fixture() -> Result<(tempfile::TempDir, Arc<Manager>, Workspace, Arc<FakeHost>)> {
    let (root, mut manager) = manager().await;
    let host = Arc::new(FakeHost::default());
    Arc::get_mut(&mut manager)
        .context("manager is shared")?
        .session_hosts = vec![host.clone()];
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "removed".into(), None, None, None)
        .await?;
    Ok((root, manager, workspace, host))
}

#[tokio::test]
async fn removal_closes_every_owned_session_and_orphan_sweep_preserves_live_owners() -> Result<()> {
    for missing in [false, true] {
        let (_root, manager, workspace, host) = fixture().await?;
        let other = manager
            .create_workspace(&workspace.repository_id, "kept".into(), None, None, None)
            .await?;
        host.add("agent", &workspace.id)?;
        host.add("shell", &workspace.id)?;
        host.add("kept", &other.id)?;
        host.add("orphan", "no-longer-recorded")?;
        if missing {
            std::fs::remove_dir_all(&workspace.path)?;
        }
        manager
            .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
            .await?;
        assert_eq!(host.closed()?, ["agent", "shell"]);
        manager.cleanup_orphan_sessions().await;
        assert_eq!(host.closed()?, ["agent", "shell", "orphan"]);
    }
    Ok(())
}

#[tokio::test]
async fn refused_removal_keeps_sessions() -> Result<()> {
    let (_root, manager, workspace, host) = fixture().await?;
    host.add("agent", &workspace.id)?;
    std::fs::write(workspace.path.join("unsaved"), "work")?;
    assert!(
        manager
            .remove_workspace(
                &workspace.id,
                UnsavedWork::default(),
                InspectionPolicy::GitOnly
            )
            .await
            .is_err()
    );
    manager.cleanup_orphan_sessions().await;
    assert!(host.closed()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn failed_session_close_does_not_block_removal_or_other_sessions_and_is_retried() -> Result<()>
{
    let (_root, manager, workspace, host) = fixture().await?;
    host.add("bad", &workspace.id)?;
    host.add("good", &workspace.id)?;
    host.fail.store(true, Ordering::Relaxed);
    manager
        .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await?;
    assert_eq!(host.closed()?, ["good"]);
    assert!(manager.find_workspace(&workspace.id).await?.is_none());
    assert!(
        manager
            .notifications(true, 100)
            .await?
            .iter()
            .any(|n| n.message.contains("close failed"))
    );
    host.fail.store(false, Ordering::Relaxed);
    manager.cleanup_orphan_sessions().await;
    assert_eq!(host.closed()?, ["good", "bad"]);
    Ok(())
}

#[tokio::test]
async fn demos_close_their_session_after_completion_or_verified_forgetting() -> Result<()> {
    use crate::{daemon::workspace::ExecutionKind, demos::Launch};

    let (_root, manager, workspace, host) = fixture().await?;
    for disconnected in [false, true] {
        let name = if disconnected {
            "disconnected"
        } else {
            "connected"
        };
        host.add(name, &workspace.id)?;
        let demo = Launch {
            id: name.into(),
            name: name.into(),
            preset: None,
            open: None,
            scheme: None,
            environment: Default::default(),
            host: "zmx".into(),
            place: name.into(),
        };
        let started = manager
            .begin_execution_running(
                &workspace.id,
                None,
                ExecutionKind::Command,
                None,
                Some(demo),
            )
            .await?;
        if disconnected {
            let execution = started.plan.id.clone();
            drop(started);
            // Disconnected stop paths call this after proving processes stopped.
            manager.forget_execution(&execution).await?;
        } else {
            // Unknown process state must leave the session open for recovery.
            assert!(
                !manager
                    .finish_execution(started.plan.id.clone(), ExecutionKind::Command, None)
                    .await?
            );
            assert!(host.closed()?.is_empty());
            assert!(
                manager
                    .finish_execution(started.plan.id, ExecutionKind::Command, Some(0))
                    .await?
            );
        }
    }
    assert_eq!(host.closed()?, ["connected", "disconnected"]);
    assert!(manager.demos(Some(&workspace.id)).await?.is_empty());
    assert!(manager.find_workspace(&workspace.id).await?.is_some());
    Ok(())
}
