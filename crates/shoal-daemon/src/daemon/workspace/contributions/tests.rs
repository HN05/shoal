use std::sync::Arc;

use crate::{
    daemon::{
        notifications::NotificationKind,
        test_support::{commit, git, manager, repository},
        workspace::Manager,
    },
    model::Repository,
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_protocol::space_names::SpaceSelector;

/// A clone registered in place and then marked as a contribution. Its
/// path is canonical, as a clone's under `root_dir` is.
async fn contribution(manager: &Arc<Manager>, root: &std::path::Path) -> Repository {
    let upstream = repository(root, "upstream");
    let clone = root.join("repo");
    git(
        root,
        &[
            "clone",
            "-q",
            upstream.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let checkout = std::fs::canonicalize(clone).unwrap();
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let id = repo.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE repositories SET contribution=1 WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.repository(&repo.id).await.unwrap()
}

async fn registered(manager: &Manager, repo: &Repository) -> bool {
    let repos = manager.repositories().await.unwrap();
    repos.iter().any(|other| other.id == repo.id)
}

#[tokio::test]
async fn a_contribution_waits_out_the_grace_period() {
    let (root, manager) = manager().await;
    let repo = contribution(&manager, root.path()).await;
    manager
        .write_local_config(&repo.id, Some("[cleanup]\ngrace_minutes = 10\n".into()))
        .await
        .unwrap();
    manager.sweep_contributions().await.unwrap();
    assert!(registered(&manager, &repo).await);
    let id = repo.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE contribution_grace SET cleanup_at=0 WHERE repository_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.sweep_contributions().await.unwrap();
    assert!(!registered(&manager, &repo).await);
}

#[tokio::test]
async fn a_borrowed_contribution_stays_available() {
    let (root, manager) = manager().await;
    let repo = contribution(&manager, root.path()).await;
    let other = repository(root.path(), "other");
    let other = manager
        .register_repository(other.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let borrower = manager
        .create_workspace(&other.id, "borrower".into(), None, None, None)
        .await
        .unwrap();
    let (id, path) = (repo.id.clone(), repo.path.display().to_string());
    manager
        .store
        .run(move |db| {
            db.execute_batch("INSERT INTO resource_pools VALUES ('global','pool','{}');")?;
            db.execute(
                "INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,created_at)
                     VALUES ('lease',?1,'global','pool','default','one',0)",
                [borrower.id],
            )?;
            db.execute(
                "INSERT INTO repository_resource_leases(lease_id,repository_id,path)
                     VALUES ('lease',?1,?2)",
                rusqlite::params![id, path],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.sweep_contributions().await.unwrap();
    assert!(registered(&manager, &repo).await);
    // No removal began, so new work in it is still accepted.
    manager.ensure_repository_available(&repo.id).await.unwrap();
    manager
        .store
        .run(|db| {
            db.execute("DELETE FROM resource_leases WHERE id='lease'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.sweep_contributions().await.unwrap();
    assert!(!registered(&manager, &repo).await);
}

#[tokio::test]
async fn new_work_is_refused_once_a_contribution_removal_begins() {
    let (root, manager) = manager().await;
    let repo = contribution(&manager, root.path()).await;
    assert!(manager.begin_contribution_removal(&repo).await.unwrap());
    manager
        .start_chat((&repo.id).into(), "chat-1".into(), "claude".into(), None)
        .await
        .unwrap_err();
    manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap_err();
    // The interrupted removal finishes on the next sweep.
    manager.sweep_contributions().await.unwrap();
    assert!(!registered(&manager, &repo).await);
}

#[tokio::test]
async fn a_contribution_goes_once_its_workspaces_and_checkpoints_are_gone() {
    let (root, manager) = manager().await;
    let repo = contribution(&manager, root.path()).await;
    let checkout = repo.path.clone();
    // Removal forgets the checkout's trust as `repo rm` does.
    let claude = manager.paths.home.join(".claude.json");
    let trusted = checkout.clone();
    let projects = serde_json::json!({"projects": {trusted.to_str().unwrap(): {"hasTrustDialogAccepted": true}}});
    std::fs::write(&claude, projects.to_string()).unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    manager.sweep_contributions().await.unwrap();
    assert!(
        registered(&manager, &repo).await,
        "a workspace keeps the contribution"
    );

    manager
        .remove_selected(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    manager.sweep_contributions().await.unwrap();
    assert!(
        registered(&manager, &repo).await,
        "a removal's checkpoint keeps it"
    );

    manager
        .store
        .run(|db| {
            db.execute_batch(
                "UPDATE checkpoints SET expires_at=0; UPDATE removed_spaces SET expires_at=0;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_checkpoints().await.unwrap();
    // Upstream moving on after the last workspace leaves nothing here unkept.
    let upstream = root.path().join("upstream");
    std::fs::write(upstream.join("later"), "later\n").unwrap();
    commit(&upstream, "later");
    // Work left in the checkout, such as a chat's, keeps it too.
    let left = checkout.join("notes");
    std::fs::write(&left, "unsaved\n").unwrap();
    manager.sweep_contributions().await.unwrap();
    assert!(registered(&manager, &repo).await, "checkout work keeps it");
    std::fs::remove_file(&left).unwrap();
    manager.sweep_contributions().await.unwrap();
    assert!(!registered(&manager, &repo).await);
    assert!(!checkout.exists());
    // Debug builds, such as this test's, leave user files alone unless told.
    let config = std::fs::read_to_string(&claude).unwrap();
    let kept = config.contains(trusted.to_str().unwrap());
    assert_eq!(kept, !crate::build::changes_user_files(), "{config}");
    let notifications = manager.notifications(false, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == NotificationKind::RepositoryRemoved)
    );
}

#[tokio::test]
async fn an_ended_chats_files_keep_a_contribution_until_they_expire() {
    let (root, manager) = manager().await;
    let repo = contribution(&manager, root.path()).await;
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let report = root.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    manager
        .add_attached_files(
            &SpaceSelector::id(&chat.id),
            vec![report.into()],
            &crate::daemon::workspace_files::Attacher::User,
        )
        .await
        .unwrap();
    manager.end_executions(&chat.id).await;
    manager
        .remove_selected(
            &SpaceSelector::id(&chat.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    manager.sweep_contributions().await.unwrap();
    assert!(
        registered(&manager, &repo).await,
        "the removed chat's files and removal record keep it"
    );

    manager
        .store
        .run(|db| {
            db.execute("UPDATE removed_space_files SET expires_at=0", [])?;
            db.execute("UPDATE removed_spaces SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.sweep_contributions().await.unwrap();
    assert!(!registered(&manager, &repo).await);
}

#[tokio::test]
async fn a_held_contribution_stays_until_its_connection_closes() {
    use crate::protocol::{Body, Response};
    let (root, manager) = manager().await;
    let repo = contribution(&manager, root.path()).await;
    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let guard = Arc::new(tokio::sync::RwLock::new(())).read_owned().await;
    let held = tokio::spawn(crate::daemon::contribution_holds::hold(
        server,
        1,
        manager.clone(),
        repo.id.clone(),
        guard,
    ));
    let response: Response = crate::protocol::read(&mut client).await.unwrap();
    assert!(matches!(response.body, Body::Ok), "{:?}", response.body);
    // The fixture's grace period is over at once.
    manager.sweep_contributions().await.unwrap();
    assert!(registered(&manager, &repo).await);
    drop(client);
    held.await.unwrap().unwrap();
    manager.sweep_contributions().await.unwrap();
    assert!(!registered(&manager, &repo).await);
}

#[tokio::test]
async fn a_removed_contribution_names_the_fork_it_leaves() {
    let (root, manager) = manager().await;
    let repo = contribution(&manager, root.path()).await;
    let fork = repository(root.path(), "fork");
    let fork = fork.to_str().unwrap();
    git(&repo.path, &["remote", "add", "fork", fork]);
    manager.sweep_contributions().await.unwrap();
    assert!(!registered(&manager, &repo).await);
    let notifications = manager.notifications(false, 10).await.unwrap();
    let removed = notifications
        .iter()
        .find(|notification| notification.kind == NotificationKind::RepositoryRemoved)
        .unwrap();
    assert!(removed.message.contains(fork), "{}", removed.message);
}

#[tokio::test]
async fn a_restarted_daemon_keeps_contributions_while_their_holds_resume() {
    let (root, manager) = manager().await;
    let repo = contribution(&manager, root.path()).await;
    let started = tokio::time::Instant::now();
    let holds = &manager.contribution_holds;
    holds.await_resumes(started + std::time::Duration::from_secs(3600));
    manager.sweep_contributions().await.unwrap();
    assert!(registered(&manager, &repo).await);
    holds.await_resumes(started);
    manager.sweep_contributions().await.unwrap();
    assert!(!registered(&manager, &repo).await);
}

#[tokio::test]
async fn a_scoped_caller_resumes_a_hold_only_where_it_may_administer() {
    use crate::daemon::{scope::Caller, test_support::chat};
    use crate::protocol::RepositoryMethod;
    let (root, manager) = manager().await;
    let repo = contribution(&manager, root.path()).await;
    let config = "[modes.chat.permissions]\nadmin = 'deny'\n\
                  [modes.plan.permissions]\nadmin = 'propose'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    for (mode, allowed) in [("chat", false), ("plan", true)] {
        let space = chat(&manager, mode, mode, Some(&repo), None).await;
        let caller = Caller {
            space_id: space.id,
            execution: None,
            granted: None,
            read_only: false,
        };
        let token = format!("scope-{mode}");
        manager
            .executions
            .issue_scope(token.clone().into(), caller)
            .await;
        let mut method = RepositoryMethod::HoldContribution {
            repository: repo.id.clone(),
        };
        let authorized =
            crate::daemon::scope::authorize(&manager, Some(&token.into()), &mut method).await;
        assert_eq!(authorized.is_ok(), allowed, "{mode}");
    }
}
