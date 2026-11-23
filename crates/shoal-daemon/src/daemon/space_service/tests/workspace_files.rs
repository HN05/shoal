use crate::daemon::scope::Access;
use crate::removal::{InspectionPolicy, UnsavedWork};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::assignments::Done;
use std::time::Duration;

use crate::daemon::workspace_files::{Attacher, FileSource};
use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        scope::Caller,
        test_support::{manager, repository},
    },
    execution::recovery,
    model::Workspace,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// Whether permissions bind this test process; root reads and deletes past
/// them, as in CI's containers.
fn permissions_bind() -> bool {
    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    (unsafe { libc::geteuid() }) != 0
}

async fn workspace(temp: &tempfile::TempDir, manager: &Manager) -> Workspace {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "receiver".into(), None, None, None)
        .await
        .unwrap()
}

/// The caller that the processes of a command registered by `caller` in
/// `workspace` send requests as, with the command's execution.
async fn command_caller(
    manager: &Manager,
    workspace: &Workspace,
    caller: Option<&Caller>,
) -> Caller {
    let token = manager
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            caller,
        )
        .await
        .unwrap()
        .plan
        .scope_token;
    manager
        .coordination
        .caller(manager, &token)
        .await
        .unwrap()
        .unwrap()
}

/// The caller that the processes of a tracked agent `name` in the space
/// `id` named `space` send requests as.
async fn agent_caller(manager: &Manager, id: &SpaceId, space: &SpaceName, name: &str) -> Caller {
    let token = manager
        .agents
        .begin_agent_execution(manager, id.as_str())
        .await
        .unwrap()
        .plan
        .scope_token;
    let caller = manager
        .coordination
        .caller(manager, &token)
        .await
        .unwrap()
        .unwrap();
    manager
        .agents
        .track_agent(
            manager,
            caller.execution_id().unwrap(),
            &shoal_protocol::executions::TrackedAgent::named(name),
            id,
            space,
            true,
            Duration::ZERO,
        )
        .await;
    caller
}

/// Start a chat named `name` in the repository of `workspace`.
async fn chat(
    manager: &std::sync::Arc<Manager>,
    workspace: &Workspace,
    name: &str,
) -> crate::model::Chat {
    manager
        .start_chat(
            workspace.repository().clone().into(),
            name.into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap()
}

mod events;
#[tokio::test]
async fn files_from_outside_any_agent_are_the_users() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    assert_eq!(
        manager.spaces.attacher(&*manager, None).await,
        Attacher::User
    );
    // A workspace terminal or a command, scoped without an agent.
    let terminal = Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    assert_eq!(
        manager.spaces.attacher(&*manager, Some(&terminal)).await,
        Attacher::User
    );
    let command = command_caller(&manager, &workspace, None).await;
    assert_eq!(
        manager.spaces.attacher(&*manager, Some(&command)).await,
        Attacher::User
    );

    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    let attacher = manager.spaces.attacher(&*manager, None).await;
    let [added] = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![file.into()],
            &attacher,
        )
        .await
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(added.attached_by, Attacher::User);
    assert_eq!(
        serde_json::to_value(&added).unwrap()["attached_by"],
        serde_json::json!({"kind": "user"})
    );
}

#[tokio::test]
async fn an_agents_files_name_its_kind_and_session() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let caller = agent_caller(&manager, &workspace.id, &workspace.name, "happy claude").await;
    let execution = caller.execution_id().unwrap().to_owned();
    // An agent that has not reported its session yet.
    assert_eq!(
        manager.spaces.attacher(&*manager, Some(&caller)).await,
        Attacher::Agent {
            agent: "happy-claude".into(),
            execution: execution.clone(),
            session: None,
        }
    );
    recovery::save_session(&manager.paths, &workspace.id, &execution, &"s1".into()).unwrap();
    let attacher = manager.spaces.attacher(&*manager, Some(&caller)).await;
    assert_eq!(
        serde_json::to_value(&attacher).unwrap(),
        serde_json::json!({
            "kind": "agent",
            "agent": "happy-claude",
            "execution": execution,
            "session": "s1",
        })
    );

    let (notes, log) = (temp.path().join("notes.md"), temp.path().join("crash.log"));
    std::fs::write(&notes, "plan").unwrap();
    std::fs::write(&log, "panic").unwrap();
    manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![notes.into()],
            &attacher,
        )
        .await
        .unwrap();
    manager
        .coordination
        .send_space_message(
            &*manager,
            Some(&caller),
            &SpaceSelector::id(&workspace.id),
            "See the log".into(),
            vec![log.into()],
        )
        .await
        .unwrap();
    // A command the agent runs with `shoal run`, and one that runs.
    let command = command_caller(&manager, &workspace, Some(&caller)).await;
    let nested = command_caller(&manager, &workspace, Some(&command)).await;
    assert_eq!(
        manager.spaces.attacher(&*manager, Some(&nested)).await,
        attacher
    );
    // A reattaching command keeps the agent it runs under.
    let id = nested.execution_id().unwrap().to_owned();
    let recorded: Option<ExecutionId> = manager
        .store
        .run(move |db| {
            Ok(db.query_row(
                "SELECT agent_execution FROM executions WHERE id=?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(recorded, Some(execution.clone()));
    // A command reattaching after a restart before its agent, and one it
    // runs then.
    manager
        .agents
        .executions()
        .with_agents(|agents| agents.remove(&execution))
        .await
        .unwrap();
    assert_eq!(
        manager.spaces.attacher(&*manager, Some(&nested)).await,
        attacher
    );
    let later = command_caller(&manager, &workspace, Some(&nested)).await;
    assert_eq!(
        manager.spaces.attacher(&*manager, Some(&later)).await,
        attacher
    );
    let files = manager
        .spaces
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(
        files.iter().all(|file| file.attached_by == attacher),
        "{files:?}"
    );
}

#[tokio::test]
async fn results_name_the_agent_that_handed_them_back() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let caller = agent_caller(&manager, &workspace.id, &workspace.name, "codex").await;
    let attacher = manager.spaces.attacher(&*manager, Some(&caller)).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "done").unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                results: vec![report.into()],
                ..Done::default()
            },
            &attacher,
        )
        .await
        .unwrap();
    let files = manager
        .spaces
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let [result] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(result.source, FileSource::Result);
    assert_eq!(
        result.attached_by,
        Attacher::Agent {
            agent: "codex".into(),
            execution: caller.execution_id().unwrap().clone(),
            session: None,
        }
    );
}

#[tokio::test]
async fn added_files_need_no_message() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("config.toml");
    std::fs::write(&file, "port = 1").unwrap();
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![file.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let [copy] = added.as_slice() else {
        panic!("{added:?}");
    };
    assert_eq!(copy.source, FileSource::Added);
    assert_eq!(
        std::fs::read_to_string(copy.path().unwrap()).unwrap(),
        "port = 1"
    );
    // Adding it again keeps the copy; sending it is a separate copy.
    manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![file.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .coordination
        .send_space_message(
            &*manager,
            None,
            &SpaceSelector::id(&workspace.id),
            String::new(),
            vec![file.into()],
        )
        .await
        .unwrap();
    let files = manager
        .spaces
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let sources: Vec<_> = files.iter().map(|file| file.source).collect();
    assert_eq!(sources, [FileSource::Added, FileSource::Message]);
    assert!(
        manager
            .agents
            .agent_messages(&*manager, &SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .iter()
            .all(|message| message.attachments().len() == 1),
        "adding sends no message"
    );
}

#[tokio::test]
async fn results_outlive_removal_as_long_as_its_record() {
    use crate::daemon::{
        notifications::AlertKind,
        scope::{self, Caller},
    };
    use crate::protocol::FileMethod;
    let (temp, manager) = manager().await;
    let parent = workspace(&temp, &manager).await;
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "audit".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    let completion = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&child.id),
            Done {
                results: vec![report.into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(completion.results[0].name, "report.md");
    let result = manager
        .spaces
        .attached_files(&SpaceSelector::id(&child.id))
        .await
        .unwrap()[0]
        .path()
        .unwrap()
        .to_path_buf();
    assert_eq!(std::fs::read_to_string(&result).unwrap(), "No leaks");
    assert!(
        manager
            .notifications
            .list(true, 50)
            .await
            .unwrap()
            .iter()
            .any(|n| n.kind == AlertKind::SpaceDone && n.message.contains("report.md")),
    );

    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&child.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    manager.spaces.expire_results(&*manager).await.unwrap();
    let files = manager
        .spaces
        .attached_files(&SpaceSelector::id(&child.id))
        .await
        .unwrap();
    let [kept] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(kept.source, FileSource::Result);
    assert!(kept.expires_at.is_some(), "{kept:?}");
    assert!(result.exists());

    // The parent's processes reach the removed child's results; others do not.
    let sibling = manager
        .create_workspace(parent.repository(), "sibling".into(), None, None, None)
        .await
        .unwrap();
    for (token, workspace, allowed) in [("parent", &parent, true), ("sibling", &sibling, false)] {
        manager
            .agents
            .executions()
            .issue_scope(
                token.into(),
                Caller {
                    space_id: workspace.id.clone(),
                    execution: None,
                    granted: None,
                    access: Access::Full,
                    vouched: None,
                },
            )
            .await;
        let mut list = crate::protocol::Operation::from(FileMethod::ListFiles {
            space: SpaceSelector::id(&child.id),
        });
        let authorized = scope::authorize(&manager, Some(&token.into()), &mut list).await;
        assert_eq!(authorized.is_ok(), allowed, "{token}");
    }

    let id = child.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM removed_space_files WHERE space_id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.spaces.expire_results(&*manager).await.unwrap();
    assert!(!result.exists());
    assert!(
        manager
            .spaces
            .attached_files(&SpaceSelector::id(&child.id))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn results_stay_reachable_when_a_workspace_between_goes() {
    use crate::daemon::scope::{self, Caller};
    use crate::protocol::FileMethod;
    let (temp, manager) = manager().await;
    let top = workspace(&temp, &manager).await;
    let middle = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&top.id),
            "middle".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let bottom = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&middle.id),
            "bottom".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&bottom.id),
            Done {
                results: vec![report.into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    for removed in [&bottom, &middle] {
        manager
            .spaces
            .remove_selected(
                &*manager,
                &SpaceSelector::id(&removed.id),
                crate::removal::UnsavedWork::default(),
                crate::removal::InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
    }
    manager
        .agents
        .executions()
        .issue_scope(
            "top".into(),
            Caller {
                space_id: top.id.clone(),
                execution: None,
                granted: None,
                access: Access::Full,
                vouched: None,
            },
        )
        .await;
    let mut list = crate::protocol::Operation::from(FileMethod::ListFiles {
        space: SpaceSelector::id(&bottom.id),
    });
    scope::authorize(&manager, Some(&"top".into()), &mut list)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_chats_files_outlive_it_for_the_retention_period() {
    use crate::{
        daemon::scope::{self, Caller},
        protocol::FileMethod,
    };
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let chat = chat(&manager, &workspace, "planning").await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&chat.id),
            vec![report.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let copy = added[0].path().unwrap().to_path_buf();
    let attachments = workspace.path.parent().unwrap().join(".attachments");
    assert!(copy.starts_with(attachments.join(&chat.id)));
    let files = manager
        .spaces
        .attached_files(&SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    assert_eq!(files, added);
    assert_eq!(
        (
            files[0].space.name.as_str(),
            files[0].space.kind,
            files[0].expires_at
        ),
        ("planning", crate::model::SpaceKind::Chat, None)
    );
    // Deleting the files of every idle workspace spares a running chat's.
    assert!(
        manager
            .spaces
            .remove_idle_files(&*manager)
            .await
            .unwrap()
            .is_empty()
    );

    // A workspace's processes reach no chat's files.
    manager
        .agents
        .executions()
        .issue_scope(
            "worker".into(),
            Caller {
                space_id: workspace.id.clone(),
                execution: None,
                granted: None,
                access: Access::Full,
                vouched: None,
            },
        )
        .await;
    let mut list = crate::protocol::Operation::from(FileMethod::ListFiles {
        space: SpaceSelector::id(&chat.id),
    });
    assert!(
        scope::authorize(&manager, Some(&"worker".into()), &mut list)
            .await
            .is_err()
    );

    manager.agents.end_executions(&*manager, &chat.id).await;
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&chat.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    manager.spaces.expire_results(&*manager).await.unwrap();
    let files = (manager.spaces.attached_files(&SpaceSelector::id(&chat.id)))
        .await
        .unwrap();
    let [kept] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(kept.source, FileSource::Added);
    assert!(kept.expires_at.is_some(), "{kept:?}");
    assert!(copy.exists());
    // A removed chat takes no more files.
    let late = temp.path().join("late.md");
    std::fs::write(&late, "Late").unwrap();
    assert!(
        manager
            .spaces
            .add_attached_files(
                &*manager,
                &SpaceSelector::id(&chat.id),
                vec![late.into()],
                &Attacher::User
            )
            .await
            .is_err()
    );

    manager
        .store
        .run(|db| {
            db.execute("UPDATE removed_space_files SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    // Files expiry cannot delete stay listed as the chat's.
    let directory = attachments.join(&chat.id);
    if permissions_bind() {
        use std::os::unix::fs::PermissionsExt;
        let set = |mode| {
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        set(0o500);
        let failed = manager.spaces.expire_results(&*manager).await;
        set(0o700);
        assert!(failed.is_err());
        let files = (manager.spaces.attached_files(&SpaceSelector::id(&chat.id)))
            .await
            .unwrap();
        assert_eq!(files[0].space.name, "planning", "{files:?}");
        assert!(files[0].expires_at.is_some(), "{files:?}");
    }
    manager.spaces.expire_results(&*manager).await.unwrap();
    assert!(!copy.exists());
    assert!(
        manager
            .spaces
            .attached_files(&manager.named("planning").await)
            .await
            .is_err()
    );
    let left: i64 = manager
        .store
        .run(|db| {
            Ok(
                db.query_row("SELECT COUNT(*) FROM removed_space_files", [], |row| {
                    row.get(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_chat_without_a_repository_keeps_its_files_in_root_dir() {
    let (temp, manager) = manager().await;
    let new = crate::daemon::chats::NewChat {
        priority: None,
        narrowing: Default::default(),
        name: "master".into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        origin: None,
        parent: None,
    };
    let chat = manager
        .spaces
        .start_new_chat(&*manager, None, None, new)
        .await
        .unwrap();
    let chat = manager.run_chat_agent(chat, None).await.unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&chat.id),
            vec![report.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let copy = added[0].path().unwrap().to_path_buf();
    let root_dir = manager.config().root_dir(&manager.paths).unwrap();
    assert!(
        copy.starts_with(root_dir.join(".attachments")),
        "{}",
        copy.display()
    );
    assert_eq!(
        manager
            .spaces
            .attached_files(&SpaceSelector::id(&chat.id))
            .await
            .unwrap(),
        added
    );
    // Deleting the files of every idle space spares a running chat's.
    assert!(
        manager
            .spaces
            .remove_idle_files(&*manager)
            .await
            .unwrap()
            .is_empty()
    );

    manager.agents.end_executions(&*manager, &chat.id).await;
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&chat.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    manager.spaces.expire_results(&*manager).await.unwrap();
    assert!(copy.exists());
    manager
        .store
        .run(|db| {
            db.execute("UPDATE removed_space_files SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.spaces.expire_results(&*manager).await.unwrap();
    assert!(!copy.exists());
    assert!(
        manager
            .spaces
            .attached_files(&manager.named("master").await)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn files_of_a_chat_without_a_repository_stay_with_the_root_dir_it_started_in() {
    let (temp, manager) = manager().await;
    let new = crate::daemon::chats::NewChat {
        priority: None,
        narrowing: Default::default(),
        name: "master".into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        origin: None,
        parent: None,
    };
    let chat = manager
        .spaces
        .start_new_chat(&*manager, None, None, new)
        .await
        .unwrap();
    let chat = manager.run_chat_agent(chat, None).await.unwrap();
    let root_dir = manager.config().root_dir(&manager.paths).unwrap();
    let space = SpaceSelector::id(&chat.id);
    let attach = |name: &str| {
        let file = temp.path().join(name);
        std::fs::write(&file, name).unwrap();
        manager
            .spaces
            .add_attached_files(&*manager, &space, vec![file.into()], &Attacher::User)
    };
    let early = attach("early.md").await.unwrap()[0]
        .path()
        .unwrap()
        .to_path_buf();
    // root_dir changes while the chat runs, as an edit and a reload change it.
    let moved = std::fs::canonicalize(temp.path()).unwrap().join("moved");
    let config = format!("[machine]\nroot_dir = {moved:?}\n");
    crate::daemon::test_support::configure(&manager, &config).await;
    let late = attach("late.md").await.unwrap()[0]
        .path()
        .unwrap()
        .to_path_buf();
    for copy in [&early, &late] {
        assert!(
            copy.starts_with(root_dir.join(".attachments")),
            "{}",
            copy.display()
        );
    }
    // Deleting one still finds it where it is kept.
    manager
        .spaces
        .remove_attached_files(
            &*manager,
            &SpaceSelector::id(&chat.id),
            vec!["early.md".into()],
            Vec::new(),
        )
        .await
        .unwrap();
    assert!(!early.exists());

    manager.agents.end_executions(&*manager, &chat.id).await;
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&chat.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    manager
        .store
        .run(|db| {
            db.execute("UPDATE removed_space_files SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.spaces.expire_results(&*manager).await.unwrap();
    assert!(!late.exists(), "{}", late.display());
    assert!(
        manager
            .spaces
            .attached_files(&manager.named("master").await)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_chats_agent_attaches_as_an_agent_with_its_execution_and_session() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let chat = chat(&manager, &workspace, "planning").await;
    // Stopped, so the execution registered below is its agent.
    manager.agents.end_executions(&*manager, &chat.id).await;
    let caller = agent_caller(&manager, &chat.id, &chat.name, "claude").await;
    let execution = caller.execution_id().unwrap().to_owned();
    recovery::save_session(&manager.paths, &chat.id, &execution, &"s1".into()).unwrap();
    let as_agent = Attacher::Agent {
        agent: "claude".into(),
        execution: execution.clone(),
        session: Some("s1".into()),
    };
    // A chat's agent attaching to a workspace attaches as itself.
    let report = temp.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    let attacher = manager.spaces.attacher(&*manager, Some(&caller)).await;
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![report.into()],
            &attacher,
        )
        .await
        .unwrap();
    assert_eq!(added[0].attached_by, as_agent);

    // Its execution's record names it after a restart, before it reattaches.
    manager
        .agents
        .executions()
        .with_agents(|agents| agents.remove(&execution))
        .await
        .unwrap();
    assert_eq!(
        manager.spaces.attacher(&*manager, Some(&caller)).await,
        as_agent
    );
}

#[tokio::test]
async fn deleting_a_record_keeps_other_copies_of_the_same_name() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![file.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .coordination
        .send_space_message(
            &*manager,
            None,
            &SpaceSelector::id(&workspace.id),
            String::new(),
            vec![file.into()],
        )
        .await
        .unwrap();
    let deleted = manager
        .spaces
        .remove_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![],
            vec![added[0].id],
        )
        .await
        .unwrap();
    assert_eq!(deleted, added);
    let left = manager
        .spaces
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let [sent] = left.as_slice() else {
        panic!("{left:?}");
    };
    assert_eq!(sent.source, FileSource::Message);
    assert!(sent.path().unwrap().exists());
}

#[tokio::test]
async fn deleting_by_name_keeps_the_other_files() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let (log, shot) = (temp.path().join("crash.log"), temp.path().join("shot.png"));
    std::fs::write(&log, "panic").unwrap();
    std::fs::write(&shot, "image").unwrap();
    manager
        .coordination
        .send_space_message(
            &*manager,
            None,
            &SpaceSelector::id(&workspace.id),
            "See".into(),
            vec![log.clone().into(), shot.into()],
        )
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .remove_attached_files(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                vec!["missing.txt".into()],
                vec![]
            )
            .await
            .is_err()
    );
    let deleted = manager
        .spaces
        .remove_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec!["crash.log".into()],
            vec![],
        )
        .await
        .unwrap();
    assert!(!deleted[0].path().unwrap().exists());
    let left = manager
        .spaces
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let [kept] = left.as_slice() else {
        panic!("{left:?}");
    };
    assert_eq!(kept.name, "shot.png");
    assert!(kept.path().unwrap().exists());

    // Sending the same files again copies the deleted one anew.
    manager
        .coordination
        .send_space_message(
            &*manager,
            None,
            &SpaceSelector::id(&workspace.id),
            "See".into(),
            vec![log.into(), temp.path().join("shot.png").into()],
        )
        .await
        .unwrap();
    assert!(deleted[0].path().unwrap().exists());
    assert_eq!(
        manager
            .spaces
            .attached_files(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn files_sent_with_messages_are_listed_until_they_expire() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    assert!(
        manager
            .spaces
            .attached_files(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .is_empty()
    );
    // Sending the same file again keeps one record of its copy.
    for _ in 0..2 {
        manager
            .coordination
            .send_space_message(
                &*manager,
                None,
                &SpaceSelector::id(&workspace.id),
                "See the log".into(),
                vec![file.clone().into()],
            )
            .await
            .unwrap();
    }
    let files = manager
        .spaces
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let [listed] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(listed.name, "crash.log");
    assert_eq!(listed.bytes, 5);
    assert_eq!(listed.source, FileSource::Message);
    assert_eq!(listed.space.name, "receiver");
    assert_eq!(
        std::fs::read_to_string(listed.path().unwrap()).unwrap(),
        "panic"
    );

    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    // The removed workspace's files stay, listed under its ID.
    let kept = manager
        .spaces
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let [kept] = kept.as_slice() else {
        panic!("{kept:?}");
    };
    assert_eq!(
        (&kept.path().unwrap(), kept.expires_at.is_some()),
        (&listed.path().unwrap(), true)
    );
    assert!(listed.path().unwrap().exists());
    manager
        .store
        .run(|db| {
            db.execute("UPDATE removed_space_files SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.spaces.expire_results(&*manager).await.unwrap();
    assert!(!listed.path().unwrap().exists());
    let left = manager
        .store
        .run(|db| {
            Ok(db.query_row("SELECT COUNT(*) FROM space_files", [], |row| {
                row.get::<_, i64>(0)
            })?)
        })
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn titles_stay_with_their_files_until_a_new_one() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let (chart, shot, report) = (
        temp.path().join("chart.html"),
        temp.path().join("shot.png"),
        temp.path().join("report.md"),
    );
    for file in [&chart, &shot, &report] {
        std::fs::write(file, "content").unwrap();
    }
    let titled = |path: &std::path::Path, title: &str| crate::daemon::workspace_files::NewFile {
        path: path.to_owned(),
        title: Some(title.into()),
    };
    manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![titled(&chart, "Latency by region")],
            &Attacher::User,
        )
        .await
        .unwrap();
    // Adding it again without a title keeps the title.
    manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![chart.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .coordination
        .send_space_message(
            &*manager,
            None,
            &SpaceSelector::id(&workspace.id),
            String::new(),
            vec![titled(&shot, "Login screen"), report.clone().into()],
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                results: vec![titled(&report, "Audit findings")],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    let titles = |files: Vec<crate::daemon::workspace_files::AttachedFile>| -> Vec<_> {
        files
            .into_iter()
            .map(|file| (file.name, file.title))
            .collect()
    };
    let listed = manager
        .spaces
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(
        titles(listed),
        [
            ("chart.html".into(), Some("Latency by region".into())),
            ("shot.png".into(), Some("Login screen".into())),
            ("report.md".into(), None),
            ("report.md".into(), Some("Audit findings".into())),
        ]
    );
    // A new title replaces the old one.
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![titled(&chart, "Latency")],
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(
        titles(added),
        [("chart.html".into(), Some("Latency".into()))]
    );
}
