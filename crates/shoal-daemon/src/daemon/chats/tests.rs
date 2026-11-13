use super::*;
use crate::client::space_names::SpaceSelector;
use crate::paths::StateFiles;
use crate::{
    daemon::{
        events::{ChatEventKind, EventItem, EventKind},
        test_support::{manager, repository},
    },
    model::{SpaceKind, SpaceRef},
};
use shoal_refs::BranchName;

/// Chat starts, stops and removals, and workspace parent changes, in
/// journal order.
async fn chat_events(manager: &Manager) -> Vec<String> {
    manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Chat(event) => Some(format!("{} {}", event.kind, event.chat.name)),
            EventItem::Event(event) if event.details.kind == EventKind::ParentChanged => {
                let parent = event.details.parent.flatten().map(|parent| parent.id);
                Some(format!("{} {parent:?}", event.details.name))
            }
            EventItem::Event(_)
            | EventItem::File(_)
            | EventItem::Shoal(_)
            | EventItem::Gap { .. } => None,
        })
        .collect()
}

#[tokio::test]
async fn chats_stop_with_their_process_and_keep_the_workspaces_they_open() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let ending = manager
        .start_chat(
            repo.id.clone().into(),
            "stopping".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let running = manager
        .start_chat(
            repo.id.clone().into(),
            "running".into(),
            "codex".into(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        (ending.name.as_str(), running.name.as_str()),
        ("stopping", "running")
    );
    assert_eq!(
        manager.chats().await.unwrap(),
        [ending.clone(), running.clone()]
    );

    manager
        .assign_parent(&workspace.id, Some(ending.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        manager
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .parent
            .map(|parent| parent.id),
        Some(ending.id.clone())
    );
    // An unknown chat gives the workspace to the user rather than failing.
    manager
        .assign_parent(&workspace.id, Some("gone".into()))
        .await
        .unwrap();
    assert_eq!(
        manager
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .parent
            .map(|parent| parent.id),
        None
    );
    manager
        .assign_parent(&workspace.id, Some(ending.id.clone()))
        .await
        .unwrap();

    // The chat stops with its agent, waiting for `shoal resume` with its
    // workspace and its name.
    manager.end_executions(&ending.id).await;
    let worker = SpaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        kind: SpaceKind::Workspace,
        branch: workspace.branch.clone(),
    };
    let stopped = Chat {
        running: false,
        children: vec![worker],
        ..ending.clone()
    };
    assert_eq!(
        manager.chats().await.unwrap(),
        [stopped.clone(), running.clone()]
    );
    assert_eq!(
        manager
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .parent
            .map(|parent| parent.id),
        Some(ending.id.clone())
    );
    assert_eq!(
        chat_events(&manager).await,
        [
            format!("{} stopping", ChatEventKind::Started),
            format!("{} running", ChatEventKind::Started),
            format!("worker {:?}", Some(&ending.id)),
            "worker None".to_owned(),
            format!("worker {:?}", Some(&ending.id)),
        ]
    );
}

#[tokio::test]
async fn a_chat_keeps_the_session_its_agent_reports() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.into(), "reporter".into(), "claude".into(), None)
        .await
        .unwrap();
    assert_eq!(chat.session, None);
    let reported = manager
        .set_agent_state(
            &manager.named("reporter").await,
            crate::state::AgentState::Idle,
            None,
            Some("session-1".into()),
        )
        .await
        .unwrap();
    assert_eq!(reported.state, crate::state::AgentState::Idle);
    // A report without a session, or with another, keeps the latest one.
    for session in [None, Some("session-2".into())] {
        manager
            .set_agent_state(
                &SpaceSelector::id(&chat.id),
                crate::state::AgentState::Idle,
                None,
                session,
            )
            .await
            .unwrap();
    }
    let chats = manager.chats().await.unwrap();
    assert_eq!(chats[0].session, Some("session-2".into()));
    manager
        .set_agent_state(
            &SpaceSelector::id(&chat.id),
            crate::state::AgentState::Idle,
            None,
            Some("../escape".into()),
        )
        .await
        .unwrap_err();
}

#[tokio::test]
async fn a_chat_shows_its_agents_turn_state_until_it_stops() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let shown = || async {
        let chats = manager.chats().await.unwrap();
        chats[0].agent_state.map(|status| status.state)
    };
    assert_eq!(shown().await, None);
    let selected = SpaceSelector::id(&chat.id);
    let agent = manager.workspace_executions(&chat.id).await.unwrap()[0]
        .id
        .clone();
    let report = |state| manager.set_agent_state(&selected, state, Some(agent.clone()), None);
    report(crate::state::AgentState::Waiting).await.unwrap();
    assert_eq!(shown().await, Some(crate::state::AgentState::Waiting));

    // The state ends with the chat's agent, and a report arriving after
    // the stop is not recorded.
    manager.end_executions(&chat.id).await;
    assert!(!manager.chats().await.unwrap()[0].running);
    assert_eq!(shown().await, None);
    report(crate::state::AgentState::Working).await.unwrap();
    assert_eq!(shown().await, None);
}

#[tokio::test]
async fn chats_and_workspaces_may_share_a_name() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let start =
        |name: &str| manager.start_chat(repo.id.clone().into(), name.into(), "claude".into(), None);

    // A chat takes a workspace's name or another chat's, unchanged.
    let first = start("worker").await.unwrap();
    let second = start("worker").await.unwrap();
    assert_eq!(
        (first.name.as_str(), second.name.as_str()),
        ("worker", "worker")
    );
    assert_ne!(first.id, second.id);

    // A workspace on another branch keeps the name its branch gives, and a
    // branch rename leaves the name as it was.
    let talk = manager
        .create_workspace(&repo.id, "talk".into(), None, None, None)
        .await
        .unwrap();
    assert_eq!(talk.name, "talk");
    let renamed = manager
        .rename_workspace(&SpaceSelector::id(&worker.id), "renamed", None)
        .await
        .unwrap();
    assert_eq!(
        (
            renamed.name.as_str(),
            renamed.branch.as_ref().map(BranchName::as_str)
        ),
        ("worker", Some("renamed"))
    );
}

#[tokio::test]
async fn a_renamed_chat_keeps_its_files() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let start = |name: &str| {
        manager.start_chat(
            repo.id.clone().into(),
            name.into(),
            "happy-claude".into(),
            None,
        )
    };
    let talk = start("talk").await.unwrap();
    start("other").await.unwrap();
    let files = manager.paths.chat_files(&talk);
    std::fs::create_dir_all(files.parent().unwrap()).unwrap();
    std::fs::write(files.with_extension("log"), "talk log").unwrap();

    let rename = |chat: &str, name: &str| manager.rename_chat(chat.into(), name.into());
    assert!(rename("gone", "topic").await.is_err());
    assert_eq!(
        rename(talk.id.as_str(), "talk").await.unwrap(),
        RenamedChat {
            chat: talk.clone(),
            previous_name: "talk".into(),
        }
    );

    // The reply names the chat as it was before.
    let renamed = rename(talk.id.as_str(), "topic").await.unwrap();
    assert_eq!(
        renamed,
        RenamedChat {
            chat: Chat {
                name: "topic".into(),
                ..talk.clone()
            },
            previous_name: "talk".into(),
        }
    );
    assert!(manager.chats().await.unwrap().contains(&renamed.chat));
    assert_eq!(manager.paths.chat_files(&renamed.chat), files);
    assert_eq!(
        std::fs::read_to_string(files.with_extension("log")).unwrap(),
        "talk log"
    );

    // The old name is free, and the rename is journaled.
    start("talk").await.unwrap();
    assert_eq!(
        chat_events(&manager).await,
        [
            format!("{} talk", ChatEventKind::Started),
            format!("{} other", ChatEventKind::Started),
            format!("{} topic", ChatEventKind::Renamed),
            format!("{} talk", ChatEventKind::Started),
        ]
    );
}

#[tokio::test]
async fn a_chat_has_its_own_tmpdir_until_it_is_removed() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let tmp_dir = chat.tmp_dir.clone().unwrap();
    let directory = repo.workspaces_dir.clone().unwrap();
    assert_eq!(tmp_dir, directory.join(".tmp").join(&chat.id));
    let mode = std::fs::metadata(&tmp_dir).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    assert_eq!(
        crate::env::chat_environment(&chat, &"token".into(), &manager.paths)[crate::env::TMPDIR],
        tmp_dir.as_os_str()
    );
    std::fs::write(tmp_dir.join("scratch"), "garbage").unwrap();

    // A stopped chat keeps it.
    manager.end_executions(&chat.id).await;
    assert!(!manager.chats().await.unwrap()[0].running);
    assert!(tmp_dir.exists());
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    assert!(manager.chats().await.unwrap().is_empty());
    assert!(!tmp_dir.exists());

    // A chat that cannot have one is forgotten at once.
    std::fs::remove_dir_all(directory.join(".tmp")).unwrap();
    std::fs::create_dir(directory.join(".tmp")).unwrap();
    manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap_err();
    assert!(manager.chats().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_removed_chat_takes_its_executions_restores_and_recovery_records() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    // Its agent runs while a stopped one waits for its restore.
    let (id, repository) = (chat.id.clone(), repo.id.clone());
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO agent_queue(id,kind,repository_id,name,agent,execution_id,state,created_at,space_id)
                    VALUES ('restore','restore',?2,'talk','claude','stopped','queued',1,?1)",
                rusqlite::params![id, repository],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let state = manager.paths.space_state(&chat.id);
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("stopped.recovery.json"), "{}").unwrap();

    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    let left = manager
        .store
        .run(|db| {
            Ok(db.query_row(
                "SELECT (SELECT COUNT(*) FROM executions) + (SELECT COUNT(*) FROM agent_queue)",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(left, 0);
    assert!(!state.exists());
}

#[tokio::test]
async fn removing_a_chat_removes_the_workspaces_below_it_first_and_stops_its_agent() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let opened = manager
        .create_workspace_below(
            repo.id.clone().into(),
            "opened".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    let split = manager
        .split_workspace(
            &SpaceSelector::id(&opened.id),
            "split".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();

    // A removal the user confirmed for fewer spaces than are below it takes
    // none of them, and the chat stays.
    manager
        .remove_chat(&manager.named("talk").await, Some(vec![opened.id.clone()]))
        .await
        .unwrap_err();
    assert!(manager.workspace_by_id(&split.id).await.is_ok());
    assert!(manager.chat(&manager.named("talk").await).await.is_ok());

    let removed = manager
        .remove_chat(&manager.named("talk").await, None)
        .await
        .unwrap();
    assert_eq!(removed.chat.id, chat.id);
    assert_eq!(removed.children, ["split", "opened"]);
    assert!(manager.workspace_by_id(&opened.id).await.is_err());
    assert!(manager.workspace_by_id(&split.id).await.is_err());
    assert!(manager.workspace_by_id(&other.id).await.is_ok());
    assert!(manager.chats().await.unwrap().is_empty());
    // The removal took the chat's agent.
    assert!(manager.running_agents().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_workspace_whose_creation_fails_stays_below_the_chat_that_opened_it() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    // A base that does not resolve fails the worktree after the record.
    manager
        .create_workspace_below(
            repo.id.clone().into(),
            "broken".into(),
            Some("does-not-exist".into()),
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap_err();
    // It is the chat's to inspect and remove, and goes with the chat.
    let broken = manager
        .workspace(&manager.named("broken").await)
        .await
        .unwrap();
    assert_eq!(broken.state, crate::state::WorkspaceState::Failed);
    assert_eq!(broken.parent.map(|parent| parent.id), Some(chat.id));
}

#[tokio::test]
async fn a_chat_being_removed_takes_no_new_space() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    // A creation that reaches its record after the removal began is refused,
    // rather than leaving a space below the chat that the removal missed.
    let _removal = manager.chat_removals.reserve(&chat.id).unwrap();
    manager
        .create_workspace_below(
            repo.id.clone().into(),
            "late".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap_err();
    manager
        .create_commit_workspace(
            repo.id.clone().into(),
            Some(chat.id.clone()),
            crate::protocol::CommitStart::Revision(None),
            Default::default(),
        )
        .await
        .unwrap_err();
    let start = super::NewChat {
        priority: None,
        narrowing: Default::default(),
        mode: "chat".into(),
        name: "late-chat".into(),
        agent: "claude".into(),
        origin: None,
        parent: Some(chat.id.clone()),
    };
    manager
        .start_new_chat(Some(repo.id.clone().into()), None, start)
        .await
        .unwrap_err();
    assert!(manager.list_workspaces().await.unwrap().is_empty());
    let chats = manager.chats().await.unwrap();
    assert_eq!(chats.len(), 1, "{chats:?}");
}

/// A registered repository with the ready workspace `worker`.
async fn workspace(root: &std::path::Path, manager: &Manager) -> (Repository, Workspace) {
    let checkout = repository(root, "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    (repo, workspace)
}

/// A chat runs the diff itself in the path the diff base names, so the base
/// names only the workspace's own worktree.
#[tokio::test]
async fn the_diff_base_names_only_the_workspaces_own_worktree() {
    let (temp, manager) = manager().await;
    let (repo, workspace) = workspace(temp.path(), &manager).await;
    let base = manager
        .diff_base(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(base.path, workspace.path);
    // A clone at its path has the same history, so only the ownership check
    // tells it apart.
    std::fs::rename(&workspace.path, temp.path().join("moved")).unwrap();
    let clone = [repo.path.as_os_str(), workspace.path.as_os_str()].map(|p| p.to_str().unwrap());
    crate::daemon::test_support::git(temp.path(), &["clone", "-q", clone[0], clone[1]]);
    crate::daemon::test_support::git(&workspace.path, &["checkout", "-q", "-b", "worker"]);
    assert!(
        manager
            .diff_base(&SpaceSelector::id(&workspace.id))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_chat_reads_the_workspace_it_runs_in_but_does_not_change_it() {
    use crate::{
        daemon::scope::{Caller, authorize},
        forge::items::link::Selection,
        protocol::{ItemContext, ItemMethod, Operation, WorkspaceMethod},
    };
    let (temp, manager) = manager().await;
    let (repo, workspace) = workspace(temp.path(), &manager).await;
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "worker-chat".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let caller = Caller {
        space_id: chat.id.clone(),
        execution: None,
    };
    manager.executions.issue_scope("chat".into(), caller).await;
    let listed = manager.visible_workspaces(Some(&chat.id)).await.unwrap();
    let names: Vec<_> = listed.into_iter().map(|listed| listed.name).collect();
    assert_eq!(names, ["worker"]);

    let status = |space: &str| {
        Operation::from(WorkspaceMethod::SpaceStatus {
            space: space.into(),
        })
    };
    let diff = Operation::from(WorkspaceMethod::DiffBase {
        workspace: SpaceSelector::id(&workspace.id),
    });
    let items = Operation::from(ItemMethod::SelectItems {
        context: ItemContext::Workspace(SpaceSelector::id(&workspace.id)),
        selection: Selection::default(),
        elsewhere: false,
    });
    for mut read in [status(workspace.id.as_str()), diff, items] {
        authorize(&manager, Some(&"chat".into()), &mut read)
            .await
            .unwrap();
    }
    let mut change = Operation::from(WorkspaceMethod::SetBaseWorkspace {
        workspace: SpaceSelector::id(&workspace.id),
        base: None,
    });
    let refused = authorize(&manager, Some(&"chat".into()), &mut change).await;
    assert!(refused.is_err());
    let refused = authorize(
        &manager,
        Some(&"chat".into()),
        &mut status(other.id.as_str()),
    )
    .await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_chat_in_a_workspace_names_it_and_runs_only_while_its_worktree_is_there() {
    let (temp, manager) = manager().await;
    let (repo, workspace) = workspace(temp.path(), &manager).await;
    let chat = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "worker-chat".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let named = Some(WorkspaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        branch: workspace.branch.clone(),
    });
    assert_eq!(
        (&chat.repository_id, &chat.workspace),
        (&Some(repo.id.clone()), &named)
    );
    // The chat is no child of the workspace, whose agent it is not.
    let recorded = manager.workspace_by_id(&workspace.id).await.unwrap();
    assert!(recorded.children.is_empty());

    // A stopped chat keeps naming the workspace.
    manager.end_executions(&chat.id).await;
    let stopped = manager.chats().await.unwrap();
    assert!(!stopped[0].running);
    assert_eq!(stopped[0].workspace, named);

    let moved = temp.path().join("moved");
    std::fs::rename(&workspace.path, &moved).unwrap();
    let restore = || manager.begin_agent_execution(chat.id.as_str());
    assert!(restore().await.is_err());
    std::fs::rename(&moved, &workspace.path).unwrap();
    restore().await.unwrap();
    let restored = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!((restored.running, restored.workspace), (true, named));
}

#[tokio::test]
async fn a_chat_runs_only_in_a_ready_workspace_with_its_worktree() {
    let (temp, manager) = manager().await;
    let (_, workspace) = workspace(temp.path(), &manager).await;
    let selected = SpaceSelector::id(&workspace.id);
    let start =
        |name: &str| manager.start_chat_in_workspace(&selected, name.into(), "claude".into(), None);
    let moved = temp.path().join("moved");
    std::fs::rename(&workspace.path, &moved).unwrap();
    assert!(start("missing").await.is_err());
    // Another directory at its path is not its worktree.
    std::fs::create_dir(&workspace.path).unwrap();
    crate::daemon::test_support::git(&workspace.path, &["init", "-q"]);
    assert!(start("replaced").await.is_err());
    std::fs::remove_dir_all(&workspace.path).unwrap();
    std::fs::rename(&moved, &workspace.path).unwrap();

    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE workspaces SET state='failed' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(start("failed").await.is_err());
    assert!(manager.chats().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_chat_records_the_mode_its_agent_started_in() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let start = super::NewChat {
        priority: None,
        narrowing: Default::default(),
        mode: "plan".into(),
        name: "repo-plan-7".into(),
        agent: "claude".into(),
        origin: None,
        parent: None,
    };
    let chat = manager
        .start_new_chat(Some(repo.id.clone().into()), None, start)
        .await
        .unwrap();
    assert_eq!(chat.mode, "plan");
    let chats = manager.chats().await.unwrap();
    assert_eq!(chats[0].mode, "plan");
}

#[tokio::test]
async fn a_chat_s_log_shows_a_removal_that_failed() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    manager
        .create_workspace_below(
            repo.id.clone().into(),
            "opened".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    let selector = SpaceSelector::id(&chat.id);
    // Confirmed without the workspace below it, the removal fails.
    manager
        .remove_chat(&selector, Some(Vec::new()))
        .await
        .unwrap_err();

    let entries = manager
        .space_log(&selector, crate::client::log::LogFilter::Entries, 50)
        .await
        .unwrap();
    let removal: Vec<_> = (entries.into_iter())
        .filter_map(|entry| match entry {
            crate::client::log::LogEntry::Event(event) => Some(event.details.kind),
            _ => None,
        })
        .filter(|kind| matches!(kind, EventKind::RemovalStarted | EventKind::Retained))
        .collect();
    assert_eq!(removal, [EventKind::RemovalStarted, EventKind::Retained]);
}
