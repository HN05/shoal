use crate::daemon::manager::Manager;
use crate::daemon::scope::Access;
use crate::daemon::scope::Caller;
use crate::git;
use crate::model::EventCause;
use crate::model::RemovedSpace;
use shoal_refs::BranchName;
use std::fs;

use crate::{
    daemon::test_support::{commit, git, manager, repository},
    model::CheckpointCause,
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_protocol::space_names::SpaceSelector;

/// The checkpoint refs in the repository, sorted.
pub(in crate::daemon) fn checkpoint_refs(repo: &std::path::Path) -> Vec<String> {
    let mut refs: Vec<String> = git(
        repo,
        &["for-each-ref", "--format=%(refname)", "refs/shoal/"],
    )
    .lines()
    .map(str::to_owned)
    .collect();
    refs.sort();
    refs
}

async fn record(manager: &Manager, id: &str) -> Option<RemovedSpace> {
    let id = id.to_owned();
    manager
        .store
        .run(move |db| shoal_spaces::restore::records::stored(db, &id.into()))
        .await
        .unwrap()
}

#[tokio::test]
async fn removal_keeps_a_checkpoint_and_record_until_they_expire() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);
    fs::write(workspace.path.join("scratch"), "notes\n").unwrap();
    // The archive limit caps only an agent's checkpoint.
    crate::daemon::test_support::configure(&manager, "[cleanup.restore]\narchive_max_mb = 0\n")
        .await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_issue(space_id,url) VALUES (?1,'https://forge/team/repo/issues/1')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    // Its unpushed commit keeps the branch.
    assert_ne!(git(&checkout, &["branch", "--list", "topic"]), "");
    let removed = record(&manager, workspace.id.as_str()).await.unwrap();
    assert_eq!(removed.name, "topic");
    assert_eq!(
        removed.kept_workspace().unwrap().checkpoint.branch,
        Some("topic".into())
    );
    assert_eq!(
        removed.kept_workspace().unwrap().checkpoint.head,
        head.trim()
    );
    assert_eq!(
        removed.kept_workspace().unwrap().checkpoint.cause,
        CheckpointCause::Removal
    );
    assert!(!removed.kept_workspace().unwrap().checkpoint.files_omitted);
    assert_eq!(removed.cause, EventCause::Manual);
    assert_eq!(
        removed.links.issue().map(|issue| issue.url.url()),
        Some("https://forge/team/repo/issues/1")
    );
    assert_eq!(
        removed.expires_at,
        removed.kept_workspace().unwrap().checkpoint.expires_at
    );
    let reference =
        git::checkpoint::reference(&removed.kept_workspace().unwrap().checkpoint.commit);
    assert_eq!(
        git(&checkout, &["rev-parse", &reference]).trim(),
        removed.kept_workspace().unwrap().checkpoint.commit
    );
    assert_eq!(
        git(
            &checkout,
            &[
                "show",
                &format!(
                    "{}:scratch",
                    removed.kept_workspace().unwrap().checkpoint.commit
                )
            ]
        ),
        "notes\n"
    );

    // An expired record is not restorable even before the sweep deletes it.
    let id = workspace.id.clone();
    let expires_at = removed.expires_at;
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE removed_spaces SET expires_at=unixepoch() WHERE space_id=?1",
                [&id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .removed_workspaces()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        manager
            .restore_workspace(&manager.named("topic").await)
            .await
            .is_err()
    );
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE removed_spaces SET expires_at=?2 WHERE space_id=?1",
                rusqlite::params![id, expires_at],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // Expiry deletes the record, its checkpoint and its ref, and refs
    // nothing owns.
    git(
        &checkout,
        &["update-ref", "refs/shoal/checkpoints/unknown/1", "HEAD"],
    );
    manager.spaces.expire_checkpoints(&*manager).await.unwrap();
    assert!(record(&manager, workspace.id.as_str()).await.is_some());
    assert_eq!(checkpoint_refs(&checkout), [reference]);
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE removed_spaces SET expires_at=0 WHERE space_id=?1",
                [&id],
            )?;
            db.execute(
                "UPDATE checkpoints SET expires_at=0 WHERE workspace_id=?1",
                [&id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.spaces.expire_checkpoints(&*manager).await.unwrap();
    assert!(record(&manager, workspace.id.as_str()).await.is_none());
    assert!(checkpoint_refs(&checkout).is_empty());
}

#[tokio::test]
async fn restore_reopens_the_branch_with_its_files_and_issue() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);
    fs::write(workspace.path.join("work"), "edited\n").unwrap();
    fs::write(workspace.path.join("scratch"), "notes\n").unwrap();
    fs::remove_file(workspace.path.join("tracked")).unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_issue(space_id,url,title) VALUES (?1,'https://github.com/acme/repo/issues/1','Fix it')",
                [&id],
            )?;
            db.execute(
                "INSERT INTO space_items(space_id,relation,kind,url,title) VALUES
                    (?1,'opened','issue','https://github.com/acme/repo/issues/2','Follow-up'),
                    (?1,'related','pr','https://github.com/acme/repo/pull/3',NULL)",
                [&id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let status = git(&workspace.path, &["status", "--porcelain"]);
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert_eq!(manager.spaces.removed_workspaces().await.unwrap().len(), 1);

    git(
        &checkout,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/repo.git",
        ],
    );
    let restored = manager
        .restore_workspace(&manager.named("topic").await)
        .await
        .unwrap();
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    // It keeps its ID, so its log goes on where its removal ended it.
    assert_eq!(restored.opened.workspace.id, workspace.id);
    let id = workspace.id.clone();
    let kinds: Vec<String> = (manager.store)
        .run(move |db| {
            Ok(db
                .prepare(
                    "SELECT json_extract(record,'$.kind') FROM space_events
                        WHERE space_id=?1 ORDER BY id",
                )?
                .query_map([&id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("created"),
        "{kinds:?}"
    );
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("restored"),
        "{kinds:?}"
    );
    assert!(kinds.iter().any(|kind| kind == "removed"), "{kinds:?}");
    let path = &restored.opened.workspace.path;
    assert!(!restored.opened.reused);
    assert_eq!(
        restored.opened.workspace.branch().map(BranchName::as_str),
        Some("topic")
    );
    assert_eq!(git(path, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(path, &["status", "--porcelain"]), status);
    assert_eq!(fs::read_to_string(path.join("scratch")).unwrap(), "notes\n");
    let workspace = manager
        .spaces
        .workspace(&manager.named("topic").await)
        .await
        .unwrap();
    assert_eq!(
        workspace.links.issue().map(|issue| issue.url.url()),
        Some("https://github.com/acme/repo/issues/1")
    );
    let recorded = |items: &[crate::model::LinkedItem]| {
        items
            .iter()
            .map(|item| (item.url.to_string(), item.title.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        recorded(&workspace.links.opened),
        [(
            "https://github.com/acme/repo/issues/2".to_owned(),
            Some("Follow-up".to_owned())
        )]
    );
    assert_eq!(
        recorded(&workspace.links.related),
        [("https://github.com/acme/repo/pull/3".to_owned(), None)]
    );
    assert!(
        manager
            .spaces
            .removed_workspaces()
            .await
            .unwrap()
            .is_empty()
    );
    // The removal's checkpoint stays the workspace's.
    let checkpoints = manager
        .spaces
        .checkpoints(&manager.named("topic").await, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(checkpoints[0].workspace_id, workspace.id);
    assert_eq!(checkpoints[0].cause, CheckpointCause::Removal);
    // Its numbering continues after the moved checkpoints.
    let next = manager
        .spaces
        .take_checkpoint(&*manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    assert_eq!(next.number, checkpoints[0].number.get() + 1);
    assert!(
        checkpoint_refs(&checkout).contains(&git::checkpoint::reference(&checkpoints[0].commit))
    );
}

#[tokio::test]
async fn a_removal_record_keeps_links_made_until_removal_starts() {
    use crate::{forge::items::link::LinkKind, model::Relation};
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // Removal works from this snapshot, taken before the links change.
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    git(
        &checkout,
        &["remote", "add", "origin", "https://github.com/acme/repo"],
    );
    manager
        .links
        .link_item(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            LinkKind::Pr,
            &"3".parse().unwrap(),
            Relation::Related,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .links
        .link_item(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            LinkKind::Pr,
            &"4".parse().unwrap(),
            Relation::Related,
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .links
        .unlink_items(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Relation::Related,
            Some(LinkKind::Pr),
            Some(&"4".parse().unwrap()),
        )
        .await
        .unwrap();
    manager
        .links
        .link_item(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            LinkKind::Issue,
            &"7".parse().unwrap(),
            Relation::Assigned,
            Some("Report".into()),
            None,
        )
        .await
        .unwrap();
    manager
        .spaces
        .reserve_lifecycle(&workspace.id, crate::state::WorkspaceState::Removing)
        .await
        .unwrap();
    assert!(
        manager
            .links
            .unlink_items(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Relation::Assigned,
                Some(LinkKind::Issue),
                None
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .links
            .link_item(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                LinkKind::Pr,
                &"5".parse().unwrap(),
                Relation::Related,
                None,
                None,
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .links
            .unlink_items(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Relation::Related,
                None,
                None
            )
            .await
            .is_err()
    );
    manager
        .spaces
        .save_removal_record(&*manager, &workspace, EventCause::Manual, None)
        .await
        .unwrap();
    let saved = record(&manager, workspace.id.as_str()).await.unwrap();
    let urls: Vec<_> = saved
        .links
        .related
        .iter()
        .map(|item| item.url.url())
        .collect();
    assert_eq!(urls, ["https://github.com/acme/repo/pull/3"]);
    assert_eq!(
        saved.links.issue().map(|issue| issue.url.url()),
        Some("https://github.com/acme/repo/issues/7")
    );
}

#[tokio::test]
async fn a_removal_interrupted_after_deleting_files_stays_restorable() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    fs::write(workspace.path.join("scratch"), "notes\n").unwrap();
    manager
        .spaces
        .save_removal_record(&*manager, &workspace, EventCause::Idle, None)
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .removed_workspaces()
            .await
            .unwrap()
            .is_empty(),
        "the workspace still exists"
    );
    // The files go, then the daemon stops before the removal commits; the
    // next sweeps forget the deleted worktree and keep its checkpoint.
    fs::remove_dir_all(&workspace.path).unwrap();
    crate::daemon::cleanup::sweep(&manager, &mut Default::default())
        .await
        .unwrap();
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
    manager.spaces.expire_checkpoints(&*manager).await.unwrap();
    assert_eq!(manager.spaces.removed_workspaces().await.unwrap().len(), 1);

    let restored = manager
        .restore_workspace(&manager.named("topic").await)
        .await
        .unwrap();
    let path = &restored.opened.workspace.path;
    assert_eq!(fs::read_to_string(path.join("scratch")).unwrap(), "notes\n");
    assert_eq!(fs::read_to_string(path.join("work")).unwrap(), "work\n");
}

#[tokio::test]
async fn a_restored_agent_forks_the_session_another_space_holds() {
    use crate::paths::StateFiles;
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let state = manager.paths.space_state(&workspace.id);
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join("saved.recovery.json"),
        r#"{"agent":"claude","mode":"worker","stage":"stopped","session":"s1"}"#,
    )
    .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    (manager.spaces.remove_selected(
        &*manager,
        &selector,
        UnsavedWork::All,
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();
    // A chat here continues the same session meanwhile.
    let chat =
        crate::daemon::test_support::chat(&manager, "holder", "chat", Some(&repo), None).await;
    crate::daemon::test_support::report_session(&manager, &chat.id, "s1").await;

    let restored = manager.restore_workspace(&selector).await.unwrap();
    assert_eq!(restored.warnings.len(), 1, "{:?}", restored.warnings);
    assert!(restored.warnings[0].contains("holder"));
    let agents = crate::execution::recovery::stopped_agents(&manager.paths, &workspace.id).unwrap();
    let [agent] = agents.as_slice() else {
        panic!("{agents:?}");
    };
    assert!(
        agent
            .record
            .copied
            .is_some_and(|copied| copied.held_elsewhere)
    );
}

#[tokio::test]
async fn a_scoped_caller_cannot_restore_a_chat_into_a_worktree_outside_its_scope() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let own = manager
        .create_workspace(&repo.id, "own".into(), None, None, None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&other.id),
            "review".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    // The chat is below the caller's space, yet runs in another's worktree.
    let (id, parent) = (chat.id.clone(), own.id.clone());
    manager
        .store
        .write(move |tx| crate::daemon::tree::set_parent(tx, &id, Some(&parent)))
        .await
        .unwrap();
    let selector = SpaceSelector::id(&chat.id);
    manager
        .spaces
        .remove_selected(
            &*manager,
            &selector,
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let caller = Caller {
        space_id: own.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };

    let refused = (manager
        .spaces
        .ensure_restorable_by(&*manager, &selector, &caller))
    .await
    .unwrap_err();
    assert!(format!("{refused:#}").contains("the user's"), "{refused:#}");
    assert!(
        manager
            .spaces
            .restorable_spaces(&*manager, Some(&caller))
            .await
            .unwrap()
            .is_empty()
    );
}
