use crate::daemon::manager::Manager;
use crate::{
    daemon::test_support::{manager, repository},
    model::Workspace,
    removal::{InspectionPolicy, UnsavedWork},
    sessions::{Host, HostName, Origin, Session},
};
use anyhow::{Context as _, Result};
use futures_util::future::BoxFuture;
use shoal_agents::sessions::*;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
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
    remaining: Mutex<Vec<(String, SpaceId)>>,
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
    fn ensure_installed(&self) -> Result<()> {
        Ok(())
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
                .map(|(name, space)| Session {
                    name: name.clone(),
                    space: space.clone(),
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
                    crate::sessions::Owner::Space(space) => self
                        .remaining
                        .lock()
                        .map_err(|e| anyhow::anyhow!("{e}"))?
                        .iter()
                        .any(|(session, id)| session == name && id == space),
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
    let (root, manager, workspace, mut hosts) = fixture_with(&[name]).await?;
    Ok((root, manager, workspace, hosts.remove(0)))
}

/// A workspace whose daemon closes sessions through a fake host of each
/// of `names`.
async fn fixture_with(
    names: &[&'static str],
) -> Result<(
    tempfile::TempDir,
    Arc<Manager>,
    Workspace,
    Vec<Arc<FakeHost>>,
)> {
    let (root, mut manager) = manager().await;
    let hosts = (names.iter())
        .map(|name| {
            Ok(Arc::new(FakeHost {
                name: Some(name.parse()?),
                ..Default::default()
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Arc::get_mut(
        &mut Arc::get_mut(&mut manager)
            .context("manager is shared")?
            .agents,
    )
    .context("manager is shared")?
    .set_session_hosts(
        hosts
            .iter()
            .map(|host| host.clone() as Arc<dyn Host>)
            .collect(),
    );
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "removed".into(), None, None, None)
        .await?;
    Ok((root, manager, workspace, hosts))
}

#[tokio::test]
async fn removal_closes_every_owned_session_and_orphan_sweep_preserves_live_owners() -> Result<()> {
    for missing in [false, true] {
        let (_root, manager, workspace, host) = fixture().await?;
        let other = manager
            .create_workspace(workspace.repository(), "kept".into(), None, None, None)
            .await?;
        host.add("agent", workspace.id.as_str())?;
        host.add("shell", workspace.id.as_str())?;
        host.add("kept", other.id.as_str())?;
        host.add("orphan", "no-longer-recorded")?;
        if missing {
            std::fs::remove_dir_all(&workspace.path)?;
        }
        manager
            .spaces
            .remove_selected(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                UnsavedWork::All,
                InspectionPolicy::GitOnly,
            )
            .await?;
        assert_eq!(host.closed()?, ["agent", "shell"]);
        manager.agents.cleanup_orphan_sessions(&*manager).await;
        assert_eq!(host.closed()?, ["agent", "shell", "orphan"]);
    }
    Ok(())
}

#[tokio::test]
async fn refused_removal_keeps_sessions() -> Result<()> {
    let (_root, manager, workspace, host) = fixture().await?;
    host.add("agent", workspace.id.as_str())?;
    std::fs::write(workspace.path.join("unsaved"), "work")?;
    assert!(
        manager
            .spaces
            .remove_selected(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                UnsavedWork::default(),
                InspectionPolicy::GitOnly
            )
            .await
            .is_err()
    );
    manager.agents.cleanup_orphan_sessions(&*manager).await;
    assert!(host.closed()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn failed_session_close_does_not_block_removal_or_other_sessions_and_is_retried() -> Result<()>
{
    let (_root, manager, workspace, host) = fixture().await?;
    host.add("bad", workspace.id.as_str())?;
    host.add("good", workspace.id.as_str())?;
    host.fail.store(true, Ordering::Relaxed);
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await?;
    assert_eq!(host.closed()?, ["good"]);
    assert!(
        manager
            .spaces
            .find_workspace(&workspace.id)
            .await?
            .is_none()
    );
    assert!(
        manager
            .notifications
            .list(true, 100)
            .await?
            .iter()
            .any(|n| n.message.contains("close failed"))
    );
    host.fail.store(false, Ordering::Relaxed);
    manager.agents.cleanup_orphan_sessions(&*manager).await;
    assert_eq!(host.closed()?, ["good", "bad"]);
    Ok(())
}

#[tokio::test]
async fn demos_close_their_session_after_completion_or_verified_forgetting() -> Result<()> {
    use crate::{daemon::executions::ExecutionKind, demos::Launch};

    for host_name in ["zmx", "rex"] {
        let (_root, manager, workspace, host) = fixture_for(host_name).await?;
        for disconnected in [false, true] {
            let name = if disconnected {
                "disconnected"
            } else {
                "connected"
            };
            host.add(name, workspace.id.as_str())?;
            host.demo(name, name)?;
            let demo = Launch {
                id: name.into(),
                name: name.into(),
                preset: None,
                open: None,
                scheme: None,
                viewer_command: None,
                environment: Default::default(),
                origin: crate::sessions::Origin::created(host_name.parse()?, name.into()),
            };
            let started = manager
                .agents
                .begin_execution_running(
                    &*manager,
                    workspace.id.as_str(),
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
                manager
                    .agents
                    .forget_execution(&*manager, &execution)
                    .await?;
            } else {
                // Unknown process state must leave the session open for recovery.
                assert!(
                    !manager
                        .agents
                        .finish_execution(
                            &*manager,
                            started.plan.id.clone(),
                            ExecutionKind::Command,
                            None
                        )
                        .await?
                );
                assert!(host.closed()?.is_empty());
                assert!(
                    manager
                        .agents
                        .finish_execution(
                            &*manager,
                            started.plan.id,
                            ExecutionKind::Command,
                            Some(0)
                        )
                        .await?
                );
            }
        }
        assert_eq!(host.closed()?, ["connected", "disconnected"]);
        assert!(
            manager
                .demos
                .list(&*manager, Some(&SpaceSelector::id(&workspace.id)))
                .await?
                .is_empty()
        );
        assert!(
            manager
                .spaces
                .find_workspace(&workspace.id)
                .await?
                .is_some()
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_demo_closes_the_session_nested_in_its_place_with_the_place() -> Result<()> {
    use crate::{daemon::executions::ExecutionKind, demos::Launch};

    let (_root, manager, workspace, hosts) = fixture_with(&["herdr", "zmx"]).await?;
    let [herdr, zmx] = [&hosts[0], &hosts[1]];
    herdr.add("w1:p1", workspace.id.as_str())?;
    zmx.add("demo-session", workspace.id.as_str())?;
    zmx.demo("demo-session", "launch")?;
    let demo = Launch {
        id: "launch".into(),
        name: "demo".into(),
        preset: None,
        open: None,
        scheme: None,
        viewer_command: None,
        environment: Default::default(),
        origin: Origin {
            nested: Some(Box::new(Origin::created(
                HostName::Zmx,
                "demo-session".into(),
            ))),
            ..Origin::created(HostName::Herdr, "w1:p1".into())
        },
    };
    let started = manager
        .agents
        .begin_execution_running(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(demo),
        )
        .await?;
    let finished = manager
        .agents
        .finish_execution(&*manager, started.plan.id, ExecutionKind::Command, Some(0))
        .await?;
    assert!(finished);
    assert_eq!(herdr.closed()?, ["w1:p1"]);
    assert_eq!(zmx.closed()?, ["demo-session"]);
    Ok(())
}

#[tokio::test]
async fn finishing_an_old_demo_does_not_close_a_replacement_in_the_same_workspace() -> Result<()> {
    use crate::{daemon::executions::ExecutionKind, demos::Launch};

    let (_root, manager, workspace, host) = fixture().await?;
    host.add("reused", workspace.id.as_str())?;
    host.demo("reused", "old-launch")?;
    let demo = Launch {
        id: "old-launch".into(),
        name: "demo".into(),
        preset: None,
        open: None,
        scheme: None,
        viewer_command: None,
        environment: Default::default(),
        origin: crate::sessions::Origin::created(crate::sessions::HostName::Zmx, "reused".into()),
    };
    let started = manager
        .agents
        .begin_execution_running(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
            Some(demo),
        )
        .await?;
    host.demo("reused", "new-launch")?;
    manager
        .agents
        .forget_execution(&*manager, &started.plan.id)
        .await?;
    assert!(host.closed()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn failed_demo_closes_survive_restart_and_retry_without_removing_the_workspace() -> Result<()>
{
    use crate::{daemon::executions::ExecutionKind, demos::Launch};

    for replacement in [false, true] {
        let (_root, manager, workspace, host) = fixture().await?;
        host.add("bad", workspace.id.as_str())?;
        host.demo("bad", "launch")?;
        host.fail.store(true, Ordering::Relaxed);
        let demo = Launch {
            id: "launch".into(),
            name: "demo".into(),
            preset: None,
            open: None,
            scheme: None,
            viewer_command: None,
            environment: Default::default(),
            origin: crate::sessions::Origin::created(crate::sessions::HostName::Zmx, "bad".into()),
        };
        let started = manager
            .agents
            .begin_execution_running(
                &*manager,
                workspace.id.as_str(),
                None,
                ExecutionKind::Command,
                None,
                Some(demo),
            )
            .await?;
        if replacement {
            manager
                .agents
                .forget_execution(&*manager, &started.plan.id)
                .await?;
        } else {
            assert!(
                manager
                    .agents
                    .finish_execution(&*manager, started.plan.id, ExecutionKind::Command, Some(0))
                    .await?
            );
        }
        assert!(
            manager
                .demos
                .list(&*manager, Some(&SpaceSelector::id(&workspace.id)))
                .await?
                .is_empty()
        );
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
        Arc::get_mut(
            &mut Arc::get_mut(&mut reopened)
                .context("reopened manager is shared")?
                .agents,
        )
        .context("reopened manager is shared")?
        .set_session_hosts(vec![host.clone()]);
        reopened.agents.cleanup_orphan_sessions(&*reopened).await;
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
        reopened.agents.cleanup_orphan_sessions(&*reopened).await;
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
        assert!(
            reopened
                .spaces
                .find_workspace(&workspace.id)
                .await?
                .is_some()
        );
        reopened.store.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn failed_execution_deletion_does_not_commit_a_pending_close() -> Result<()> {
    use crate::{daemon::executions::ExecutionKind, demos::Launch};

    let (_root, manager, workspace, _host) = fixture().await?;
    let demo = Launch {
        id: "launch".into(),
        name: "demo".into(),
        preset: None,
        open: None,
        scheme: None,
        viewer_command: None,
        environment: Default::default(),
        origin: crate::sessions::Origin::created(crate::sessions::HostName::Zmx, "session".into()),
    };
    let started = manager
        .agents
        .begin_execution_running(
            &*manager,
            workspace.id.as_str(),
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
    assert!(
        manager
            .agents
            .forget_execution(&*manager, &started.plan.id)
            .await
            .is_err()
    );
    assert_eq!(
        manager
            .demos
            .list(&*manager, Some(&SpaceSelector::id(&workspace.id)))
            .await?
            .len(),
        1
    );
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
            .agents
            .record_owned_place(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Origin::created(HostName::Rex, (*block).into()),
            )
            .await?;
    }
    Ok((root, manager, workspace, host))
}

/// Start the chat `name` of `workspace`'s repository running in `place`,
/// with a sleep standing in for its agent.
async fn chat_in(
    manager: &std::sync::Arc<Manager>,
    workspace: &Workspace,
    name: &str,
    place: Origin,
) -> Result<(crate::model::Chat, crate::test_support::ProcessGroup)> {
    let process =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = manager
        .start_chat(
            workspace.repository().clone().into(),
            name.into(),
            "claude".into(),
            Some(place),
        )
        .await?;
    Ok((chat, process))
}

#[tokio::test]
async fn a_chats_removal_closes_the_places_shoal_created_that_it_runs_in() -> Result<()> {
    let (_root, manager, workspace, host) = rex_fixture(&["taken"]).await?;
    for block in ["opened", "taken", "users"] {
        host.add(block, workspace.id.as_str())?;
    }
    let mut chats = Vec::new();
    // Shoal opened one block for its chat, and a workspace's block and the
    // user's own each run another.
    for (name, block, owned) in [
        ("opener", "opened", true),
        ("taker", "taken", false),
        ("guest", "users", false),
    ] {
        let place = Origin {
            owned,
            ..Origin::created(HostName::Rex, block.into())
        };
        chats.push(chat_in(&manager, &workspace, name, place).await?);
    }
    // The workspace's end leaves the block its chat took.
    manager
        .agents
        .close_owned_places(&*manager, Some(workspace.id.clone()))
        .await;
    assert!(host.closed()?.is_empty());
    for (chat, _process) in &chats {
        manager
            .spaces
            .remove_selected(
                &*manager,
                &SpaceSelector::id(&chat.id),
                UnsavedWork::default(),
                InspectionPolicy::GitOnly,
            )
            .await?;
    }
    assert_eq!(host.closed()?, ["opened", "taken"]);
    assert!(manager.agents.owned_places(None).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_chat_that_fails_to_start_leaves_its_place_as_it_found_it() -> Result<()> {
    let (_root, manager, workspace, host) = rex_fixture(&["taken"]).await?;
    host.add("taken", workspace.id.as_str())?;
    host.add("opened", workspace.id.as_str())?;
    // A temporary directory Shoal does not own keeps chats from starting.
    let directory = (manager
        .repositories
        .repository(workspace.repository())
        .await?)
        .workspaces_dir
        .join(".tmp");
    if directory.exists() {
        std::fs::remove_dir_all(&directory)?;
    }
    std::fs::create_dir_all(&directory)?;
    for (name, block, owned) in [("taker", "taken", false), ("opener", "opened", true)] {
        let place = Origin {
            owned,
            ..Origin::created(HostName::Rex, block.into())
        };
        assert!(chat_in(&manager, &workspace, name, place).await.is_err());
    }
    manager.agents.cleanup_orphan_sessions(&*manager).await;
    assert!(host.closed()?.is_empty());
    let owners: Vec<_> = (manager.agents.owned_places(None).await?)
        .into_iter()
        .map(|(owner, place)| (owner, place.place))
        .collect();
    assert_eq!(owners, [(workspace.id.clone(), Some("taken".to_owned()))]);
    Ok(())
}
