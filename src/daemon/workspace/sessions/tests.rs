use super::*;
use crate::sessions::PlaceOwnership;
use crate::{
    removal::{InspectionPolicy, UnsavedWork},
    sessions::{Host, HostName, Origin, Session},
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
    name: Option<HostName>,
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
    fn name(&self) -> HostName {
        self.name.unwrap_or(HostName::Zmx)
    }
    fn sessions<'a>(&'a self, _: &'a Path) -> BoxFuture<'a, Result<Vec<Session>>> {
        Box::pin(async {
            // Herdr labels no panes, so discovery finds none.
            if self.name == Some(HostName::Herdr) {
                return Ok(Vec::new());
            }
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
        place: &'a Origin,
        owner: &'a crate::sessions::Owner,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let name = place.id();
            if !self
                .remaining
                .lock()
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .iter()
                .any(|(session, _)| session == name)
            {
                return Ok(());
            }
            // A Rex block or a Herdr pane carries no labels: the daemon's
            // record proves it Shoal's.
            let owned = matches!(self.name, Some(HostName::Rex | HostName::Herdr))
                || match owner {
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
        name: Some(name.parse()?),
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

    for host_name in ["zmx", "rex"] {
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
                origin: crate::sessions::Origin::created(host_name.parse()?, name.into()),
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
        origin: crate::sessions::Origin::created(crate::sessions::HostName::Zmx, "reused".into()),
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
            origin: crate::sessions::Origin::created(crate::sessions::HostName::Zmx, "bad".into()),
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
        origin: crate::sessions::Origin::created(crate::sessions::HostName::Zmx, "session".into()),
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

/// A Rex host whose `remaining` blocks are open, with `owned` recorded as
/// Shoal's for the workspace.
async fn rex_fixture(
    owned: &[&str],
) -> Result<(tempfile::TempDir, Arc<Manager>, Workspace, Arc<FakeHost>)> {
    let (root, manager, workspace, host) = fixture_for("rex").await?;
    for block in owned {
        manager
            .record_owned_place(
                &workspace.id,
                Origin::created(HostName::Rex, (*block).into()),
            )
            .await?;
    }
    Ok((root, manager, workspace, host))
}

#[tokio::test]
async fn removal_closes_only_the_blocks_shoal_recorded_creating() -> Result<()> {
    let (_root, manager, workspace, host) = rex_fixture(&["created"]).await?;
    host.add("created", &workspace.id)?;
    // The user's own block ran an agent, but Shoal never recorded creating it.
    host.add("users", &workspace.id)?;
    manager.close_owned_places(Some(workspace.id.clone())).await;
    assert_eq!(host.closed()?, ["created"]);
    assert!(manager.owned_places(None).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn the_sweep_retries_failed_closes_once_the_workspace_is_gone() -> Result<()> {
    let (_root, manager, workspace, host) = rex_fixture(&["bad", "live"]).await?;
    host.add("bad", &workspace.id)?;
    host.add("live", &workspace.id)?;
    host.fail.store(true, Ordering::Relaxed);
    manager.close_owned_places(Some(workspace.id.clone())).await;
    assert_eq!(host.closed()?, ["live"]);
    let left: Vec<_> = manager.owned_places(None).await?;
    assert_eq!(left.len(), 1);
    // A sweep leaves the places of a workspace that still exists.
    host.fail.store(false, Ordering::Relaxed);
    manager.cleanup_orphan_sessions().await;
    assert_eq!(host.closed()?, ["live"]);
    manager
        .store
        .run({
            let id = workspace.id.clone();
            move |db| {
                db.execute(
                    "UPDATE owned_places SET workspace_id='gone' WHERE workspace_id=?1",
                    [id],
                )?;
                Ok(())
            }
        })
        .await?;
    manager.cleanup_orphan_sessions().await;
    assert_eq!(host.closed()?, ["live", "bad"]);
    assert!(manager.owned_places(None).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn places_of_hosts_without_cleanup_are_forgotten_with_their_workspace() -> Result<()> {
    let (_root, manager, workspace, host) = fixture().await?;
    manager
        .record_owned_place(
            &workspace.id,
            Origin::created(HostName::Terminal, "w1:p1".into()),
        )
        .await?;
    manager.close_owned_places(Some(workspace.id.clone())).await;
    assert!(host.closed()?.is_empty());
    assert!(manager.owned_places(None).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn an_agent_of_another_workspace_takes_over_a_block_shoal_created() -> Result<()> {
    use crate::{daemon::workspace::ExecutionKind, sessions::PlaceClose};

    let (_root, manager, workspace, host) = rex_fixture(&["shared"]).await?;
    let taker = manager
        .create_workspace(&workspace.repository_id, "taker".into(), None, None, None)
        .await?;
    host.add("shared", &workspace.id)?;
    let _started = manager
        .begin_execution_placed(
            &taker.id,
            None,
            ExecutionKind::Command,
            None,
            None,
            Some(crate::daemon::workspace::places::ExecutionPlace {
                origin: Origin::created(HostName::Rex, "shared".into()),
                agent: true,
            }),
        )
        .await?;
    // A creation record that arrives late does not take the block back.
    manager
        .record_owned_place(
            &workspace.id,
            Origin::created(HostName::Rex, "shared".into()),
        )
        .await?;
    // The first workspace's removal and its watcher leave the block open.
    manager.close_owned_places(Some(workspace.id.clone())).await;
    assert_eq!(
        manager
            .close_recorded_place(
                &workspace.id,
                &Origin::created(HostName::Rex, "shared".into())
            )
            .await?,
        PlaceClose::OwnedElsewhere
    );
    assert!(host.closed()?.is_empty());
    // It closes with the workspace whose agent took it over.
    assert_eq!(
        manager
            .close_recorded_place(&taker.id, &Origin::created(HostName::Rex, "shared".into()))
            .await?,
        PlaceClose::Closed
    );
    assert_eq!(host.closed()?, ["shared"]);
    assert_eq!(
        manager
            .close_recorded_place(&taker.id, &Origin::created(HostName::Rex, "shared".into()))
            .await?,
        PlaceClose::NotRecorded
    );
    Ok(())
}

#[tokio::test]
async fn a_recorded_place_the_daemon_cannot_close_is_left_to_its_host() -> Result<()> {
    use crate::sessions::PlaceClose;

    let (_root, manager, workspace, host) = rex_fixture(&[]).await?;
    manager
        .record_owned_place(
            &workspace.id,
            Origin::created(HostName::Terminal, "w1:p9".into()),
        )
        .await?;
    assert_eq!(
        manager
            .close_recorded_place(
                &workspace.id,
                &Origin::created(HostName::Terminal, "w1:p9".into())
            )
            .await?,
        PlaceClose::HostCloses
    );
    assert!(host.closed()?.is_empty());
    assert!(manager.owned_places(None).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_nested_command_or_a_chat_leaves_the_owner_of_a_block() -> Result<()> {
    use crate::{daemon::workspace::ExecutionKind, sessions::PlaceClose};

    let (_root, manager, workspace, host) = rex_fixture(&["parent", "chatted"]).await?;
    let child = manager
        .create_workspace(&workspace.repository_id, "child".into(), None, None, None)
        .await?;
    host.add("parent", &workspace.id)?;
    host.add("chatted", &workspace.id)?;
    // A parent agent's `shoal run --workspace child` runs in the parent's block.
    let _nested = manager
        .begin_execution_placed(
            &child.id,
            None,
            ExecutionKind::Command,
            None,
            None,
            Some(crate::daemon::workspace::places::ExecutionPlace {
                origin: Origin::created(HostName::Rex, "parent".into()),
                agent: false,
            }),
        )
        .await?;
    assert_eq!(
        manager
            .close_recorded_place(&child.id, &Origin::created(HostName::Rex, "parent".into()))
            .await?,
        PlaceClose::OwnedElsewhere
    );
    // A chat that starts in a block takes it from the workspace.
    let mut chat_place = Origin::created(HostName::Rex, "chatted".into());
    chat_place.owned = false;
    manager
        .start_chat(
            workspace.repository_id.clone().into(),
            "chat".into(),
            "claude".into(),
            std::process::id(),
            Some(chat_place),
            false,
        )
        .await?;
    manager.close_owned_places(Some(workspace.id.clone())).await;
    assert_eq!(host.closed()?, ["parent"]);
    manager.cleanup_orphan_sessions().await;
    assert_eq!(
        manager
            .close_recorded_place(
                &workspace.id,
                &Origin::created(HostName::Rex, "chatted".into())
            )
            .await?,
        PlaceClose::OwnedElsewhere
    );
    assert_eq!(host.closed()?, ["parent"]);
    Ok(())
}

#[tokio::test]
async fn a_session_closes_with_the_workspace_its_host_labels_it_for() -> Result<()> {
    let (_root, manager, workspace, host) = fixture().await?;
    let other = manager
        .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
        .await?;
    host.add("labelled", &workspace.id)?;
    assert!(
        manager
            .place_ownership(
                &workspace.id,
                &Origin::created(HostName::Zmx, "labelled".into())
            )
            .await?
            == PlaceOwnership::Workspace
    );
    assert!(
        manager
            .place_ownership(
                &other.id,
                &Origin::created(HostName::Zmx, "labelled".into())
            )
            .await?
            != PlaceOwnership::Workspace
    );
    assert!(
        manager
            .place_ownership(
                &workspace.id,
                &Origin::created(HostName::Zmx, "unlabelled".into())
            )
            .await?
            != PlaceOwnership::Workspace
    );
    assert!(
        manager
            .place_ownership(
                &workspace.id,
                &Origin::created(HostName::Rex, "labelled".into())
            )
            .await?
            != PlaceOwnership::Workspace
    );
    Ok(())
}

#[tokio::test]
async fn panes_of_different_servers_under_one_id_are_different_places() -> Result<()> {
    use crate::daemon::workspace::ExecutionKind;

    let (_root, manager, workspace, host) = fixture_for("herdr").await?;
    let taker = manager
        .create_workspace(&workspace.repository_id, "taker".into(), None, None, None)
        .await?;
    let on = |server: &str| {
        let mut pane = Origin::created(HostName::Herdr, "w1:p1".into());
        pane.data.insert("HERDR_SOCKET_PATH".into(), server.into());
        pane
    };
    manager
        .record_owned_place(&workspace.id, on("first"))
        .await?;
    host.add("w1:p1", &workspace.id)?;
    // An agent in the user's pane of the same ID on another server takes
    // nothing over, and records nothing.
    let mut users = on("second");
    users.owned = false;
    let _started = manager
        .begin_execution_placed(
            &taker.id,
            None,
            ExecutionKind::Command,
            None,
            None,
            Some(crate::daemon::workspace::places::ExecutionPlace {
                origin: users.clone(),
                agent: true,
            }),
        )
        .await?;
    assert!(
        manager.place_ownership(&workspace.id, &on("first")).await? == PlaceOwnership::Workspace
    );
    // A variable of the pane's that names no server leaves it the same place.
    let mut shell = on("first");
    shell.data.insert("HERDR_THEME".into(), "dark".into());
    assert!(manager.place_ownership(&workspace.id, &shell).await? == PlaceOwnership::Workspace);
    assert!(manager.place_ownership(&taker.id, &on("first")).await? != PlaceOwnership::Workspace);
    assert!(manager.place_ownership(&workspace.id, &users).await? != PlaceOwnership::Workspace);
    manager.close_owned_places(Some(taker.id.clone())).await;
    assert!(host.closed()?.is_empty());
    manager.close_owned_places(Some(workspace.id.clone())).await;
    assert_eq!(host.closed()?, ["w1:p1"]);
    Ok(())
}

#[tokio::test]
async fn a_session_its_host_labels_closes_through_the_host_without_a_record() -> Result<()> {
    use crate::sessions::PlaceClose;

    let (_root, manager, workspace, host) = fixture().await?;
    let other = manager
        .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
        .await?;
    host.add("labelled", &workspace.id)?;
    let session = Origin::created(HostName::Zmx, "labelled".into());
    assert_eq!(
        manager.close_recorded_place(&other.id, &session).await?,
        PlaceClose::NotRecorded
    );
    assert!(host.closed()?.is_empty());
    assert_eq!(
        manager
            .close_recorded_place(&workspace.id, &session)
            .await?,
        PlaceClose::Closed
    );
    assert_eq!(host.closed()?, ["labelled"]);
    Ok(())
}
