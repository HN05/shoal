use super::*;
use crate::{
    removal::{InspectionPolicy, UnsavedWork},
    sessions::{Host, Session},
    test_support::{manager, repository},
};
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
    name: Option<&'static str>,
    remaining: Mutex<Vec<(String, String)>>,
    demos: Mutex<std::collections::HashMap<String, String>>,
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

    fn demo(&self, name: &str, launch: &str) -> Result<()> {
        self.demos
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .insert(name.into(), launch.into());
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
        self.name.unwrap_or("zmx")
    }
    fn sessions<'a>(&'a self, _: &'a Path) -> BoxFuture<'a, Result<Vec<Session>>> {
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

    fn close<'a>(
        &'a self,
        _: &'a Path,
        name: &'a str,
        owner: &'a crate::sessions::Owner,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if !self
                .remaining
                .lock()
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .iter()
                .any(|(session, _)| session == name)
            {
                return Ok(());
            }
            let owned = match owner {
                crate::sessions::Owner::Workspace(workspace) => self
                    .remaining
                    .lock()
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .iter()
                    .any(|(session, id)| session == name && id == workspace),
                crate::sessions::Owner::Demo(launch) => {
                    self.demos
                        .lock()
                        .map_err(|e| anyhow::anyhow!("{e}"))?
                        .get(name)
                        == Some(launch)
                }
            };
            if !owned {
                return Ok(());
            }
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
    fixture_for("zmx").await
}

async fn fixture_for(
    name: &'static str,
) -> Result<(tempfile::TempDir, Arc<Manager>, Workspace, Arc<FakeHost>)> {
    let (root, mut manager) = manager().await;
    let host = Arc::new(FakeHost {
        name: Some(name),
        ..Default::default()
    });
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

    for host_name in ["zmx", "future-host"] {
        let (_root, manager, workspace, host) = fixture_for(host_name).await?;
        for disconnected in [false, true] {
            let name = if disconnected {
                "disconnected"
            } else {
                "connected"
            };
            host.add(name, &workspace.id)?;
            host.demo(name, name)?;
            let demo = Launch {
                id: name.into(),
                name: name.into(),
                preset: None,
                open: None,
                scheme: None,
                environment: Default::default(),
                host: host_name.into(),
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
    }
    Ok(())
}

#[tokio::test]
async fn finishing_an_old_demo_does_not_close_a_replacement_in_the_same_workspace() -> Result<()> {
    use crate::{daemon::workspace::ExecutionKind, demos::Launch};

    let (_root, manager, workspace, host) = fixture().await?;
    host.add("reused", &workspace.id)?;
    host.demo("reused", "old-launch")?;
    let demo = Launch {
        id: "old-launch".into(),
        name: "demo".into(),
        preset: None,
        open: None,
        scheme: None,
        environment: Default::default(),
        host: "zmx".into(),
        place: "reused".into(),
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
    host.demo("reused", "new-launch")?;
    manager.forget_execution(&started.plan.id).await?;
    assert!(host.closed()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn failed_demo_closes_survive_restart_and_retry_without_removing_the_workspace() -> Result<()>
{
    use crate::{daemon::workspace::ExecutionKind, demos::Launch};

    for replacement in [false, true] {
        let (_root, manager, workspace, host) = fixture().await?;
        host.add("bad", &workspace.id)?;
        host.demo("bad", "launch")?;
        host.fail.store(true, Ordering::Relaxed);
        let demo = Launch {
            id: "launch".into(),
            name: "demo".into(),
            preset: None,
            open: None,
            scheme: None,
            environment: Default::default(),
            host: "zmx".into(),
            place: "bad".into(),
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
        if replacement {
            manager.forget_execution(&started.plan.id).await?;
        } else {
            assert!(
                manager
                    .finish_execution(started.plan.id, ExecutionKind::Command, Some(0))
                    .await?
            );
        }
        assert!(manager.demos(Some(&workspace.id)).await?.is_empty());
        assert!(host.closed()?.is_empty());
        assert_eq!(
            manager
                .store
                .run(|db| Ok(pending::list(db, None)?.len()))
                .await?,
            1
        );
        let paths = manager.paths.clone();
        manager.store.shutdown().await;
        drop(manager);
        let mut reopened = Manager::open(paths).await?;
        Arc::get_mut(&mut reopened)
            .context("reopened manager is shared")?
            .session_hosts = vec![host.clone()];
        reopened.cleanup_orphan_sessions().await;
        assert!(host.closed()?.is_empty());
        assert_eq!(
            reopened
                .store
                .run(|db| Ok(pending::list(db, None)?.len()))
                .await?,
            1
        );
        if replacement {
            host.demo("bad", "replacement-launch")?;
        }
        host.fail.store(false, Ordering::Relaxed);
        reopened.cleanup_orphan_sessions().await;
        assert_eq!(
            host.closed()?,
            if replacement { vec![] } else { vec!["bad"] }
        );
        assert!(
            reopened
                .store
                .run(|db| pending::list(db, None))
                .await?
                .is_empty()
        );
        assert!(reopened.find_workspace(&workspace.id).await?.is_some());
        reopened.store.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn failed_execution_deletion_does_not_commit_a_pending_close() -> Result<()> {
    use crate::{daemon::workspace::ExecutionKind, demos::Launch};

    let (_root, manager, workspace, _host) = fixture().await?;
    let demo = Launch {
        id: "launch".into(),
        name: "demo".into(),
        preset: None,
        open: None,
        scheme: None,
        environment: Default::default(),
        host: "zmx".into(),
        place: "session".into(),
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
    manager
        .store
        .run(|db| {
            db.execute_batch(
                "CREATE TRIGGER refuse_execution_delete BEFORE DELETE ON executions
            BEGIN SELECT RAISE(ABORT, 'delete failed'); END;",
            )?;
            Ok(())
        })
        .await?;
    assert!(manager.forget_execution(&started.plan.id).await.is_err());
    assert_eq!(manager.demos(Some(&workspace.id)).await?.len(), 1);
    assert!(
        manager
            .store
            .run(|db| pending::list(db, None))
            .await?
            .is_empty()
    );
    Ok(())
}
