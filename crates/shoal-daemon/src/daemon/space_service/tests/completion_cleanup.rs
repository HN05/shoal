use crate::daemon::manager::Manager;
use crate::model::EventCause;
use crate::model::Space;
use crate::model::Workspace;

use crate::{
    daemon::{
        links::pr::Action,
        test_support::{commit, git, install_test_script, manager, repository},
        workspace_files::Attacher,
    },
    model::CleanupReason,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::BranchName;
use std::{fs, sync::Arc};

async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "finished".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

#[tokio::test]
async fn completion_without_cleanup_blocks_idle_and_pr_cleanup_until_requested() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .write_local_config(
            workspace.repository(),
            Some("[cleanup.done]\nenabled = false\n".into()),
        )
        .await
        .unwrap();
    manager
        .links
        .set_pr(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Action::Acknowledge,
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    assert!(
        reopened
            .spaces
            .cleanup_snapshot(&*reopened, &workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    reopened.spaces.sweep_completed(&*reopened).await.unwrap();
    reopened.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());
    reopened
        .spaces
        .mark_space_done(
            &*reopened,
            &SpaceSelector::id(&workspace.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    reopened.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
    assert!(
        reopened
            .spaces
            .completion(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    reopened.store.shutdown().await;
}

#[tokio::test]
async fn completion_retains_dirty_unpushed_and_newer_work() {
    let (_root, manager, workspace) = fixture().await;
    let file = workspace.path.join("new-work");
    fs::write(&file, "keep").unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert_retained(&manager, &workspace).await;
    commit(&workspace.path, "new-work");
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert_retained(&manager, &workspace).await;
    // Simulate a fetched remote-tracking branch preserving the completed HEAD.
    git(
        &workspace.path,
        &["update-ref", "refs/remotes/origin/finished", "HEAD"],
    );
    // A later clean, fully preserved commit must not inherit the old cleanup request.
    fs::write(&file, "newer").unwrap();
    commit(&workspace.path, "new-work");
    git(
        &workspace.path,
        &["update-ref", "refs/remotes/origin/finished", "HEAD"],
    );
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    reopened.spaces.sweep_completed(&*reopened).await.unwrap();
    assert_retained(&reopened, &workspace).await;
    assert!(
        reopened
            .spaces
            .cleanup_snapshot(&*reopened, &workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "newer");
    reopened
        .spaces
        .mark_space_done(
            &*reopened,
            &SpaceSelector::id(&workspace.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    reopened.spaces.sweep_completed(&*reopened).await.unwrap();
    assert!(!workspace.path.exists());
    // A branch whose commits a remote has goes with the workspace.
    let repo = reopened
        .repositories
        .repository(workspace.repository())
        .await
        .unwrap();
    assert_eq!(git(&repo.path, &["branch", "--list", "finished"]), "");
    reopened.store.shutdown().await;
}

/// Assert the completion stands with an error and kept the workspace.
async fn assert_retained(manager: &Manager, workspace: &Workspace) {
    let completion = manager
        .spaces
        .completion(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert!(completion.error.is_some(), "{completion:?}");
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn completion_rechecks_work_after_removal_hooks() {
    let (root, manager, workspace) = fixture().await;
    let hook = root.path().join("modify-work");
    install_test_script(&hook, "#!/bin/sh\nprintf 'preserve me' > new-work\n");
    manager
        .write_local_config(
            workspace.repository(),
            Some(format!("hooks.pre_remove = '{}'\n", hook.display())),
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert_retained(&manager, &workspace).await;
    assert_eq!(
        fs::read_to_string(workspace.path.join("new-work")).unwrap(),
        "preserve me"
    );
}

#[tokio::test]
async fn a_removal_holds_only_its_own_workspaces_gate() {
    let (root, manager, workspace) = fixture().await;
    let other = manager
        .create_workspace(workspace.repository(), "other".into(), None, None, None)
        .await
        .unwrap();
    let (started, release) = (root.path().join("started"), root.path().join("release"));
    let hook = root.path().join("wait-for-release");
    install_test_script(
        &hook,
        format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\n",
            started.display(),
            release.display()
        ),
    );
    manager
        .write_local_config(
            workspace.repository(),
            Some(format!("hooks.pre_remove = '{}'\n", hook.display())),
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    let sweep = tokio::spawn({
        let manager = manager.clone();
        async move { manager.spaces.sweep_completed(&*manager).await }
    });
    bounded(async {
        while !started.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(manager.gates.try_lock_prs([&workspace.id]).is_none());
    // Another workspace's commands do not wait for the removal.
    bounded(manager.spaces.mark_space_done(
        &*manager,
        &SpaceSelector::id(&other.id),
        Done::default(),
        &Attacher::User,
    ))
    .await
    .unwrap();
    bounded(
        manager
            .spaces
            .undo_space_done(&*manager, &SpaceSelector::id(&other.id)),
    )
    .await
    .unwrap();
    assert!(!sweep.is_finished());
    fs::write(&release, "").unwrap();
    bounded(sweep).await.unwrap().unwrap();
    assert!(!workspace.path.exists());
    assert!(other.path.exists());
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(60), future)
        .await
        .expect("operation timed out")
}

#[tokio::test]
async fn undone_withdraws_completion_without_blocking_cleanup() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    let withdrawn = manager
        .spaces
        .undo_space_done(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(withdrawn.cleanup);
    manager
        .spaces
        .undo_space_done(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap_err();
    assert!(
        manager
            .spaces
            .completion(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    // Without completion, links or holds the workspace is an idle candidate again.
    assert!(
        manager
            .spaces
            .cleanup_snapshot(&*manager, &workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    // A hold, not the withdrawn completion, keeps a merged workspace.
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            "review".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    manager
        .links
        .set_pr(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Action::Acknowledge,
        )
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            "review".into(),
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn an_unreadable_chat_completion_leaves_other_spaces_to_completion_cleanup() {
    let (_root, manager, workspace) = fixture().await;
    let chat = crate::daemon::test_support::chat(&manager, "notes", "chat", None, None).await;
    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_completion(space_id,record,cause) VALUES (?1,'{','manual')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // The chat's cleanup fails, which retains it, and the workspace goes all
    // the same.
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
    assert!(manager.spaces.chat_by_id(&chat.id).await.unwrap().is_some());
}

#[tokio::test]
async fn issue_and_watch_completion_wait_for_explicit_done() {
    let (_root, manager, workspace) = fixture().await;
    let id = workspace.id.clone();
    let record = serde_json::json!({
        "urls": ["https://github.com/team/repo/pull/1"],
        "head": null,
        "error": null,
    })
    .to_string();
    manager
        .store
        .run(move |db| {
            // Without an origin, any issue or PR lookup would record an error.
            db.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                rusqlite::params![id, record],
            )?;
            db.execute(
                "INSERT INTO space_issue(space_id,url) VALUES (?1,?2)",
                rusqlite::params![id, "https://github.com/team/repo/issues/1"],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    let inspection = manager
        .spaces
        .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(inspection.completion.is_none());
    assert!(inspection.issues[0].error.is_none());
    assert!(inspection.pr_cleanup.unwrap().error.is_none());
    // Explicit completion applies the watch's merge checks.
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    let inspection = manager
        .spaces
        .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(inspection.pr_cleanup.unwrap().error.is_some());
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn holds_retain_completion_without_turning_it_into_an_error() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            "app".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let completed = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    let completion = manager
        .spaces
        .completion(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completion.head, completed.head);
    assert!(completion.cleanup);
    assert!(completion.error.is_none());
    assert!(workspace.path.exists());
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            "app".into(),
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn threads_asked_after_done_retain_completion_and_pr_cleanup() {
    let (_root, manager, workspace) = fixture().await;
    // Done closes the threads opened before it.
    crate::daemon::test_support::ask(&manager, &workspace.id, "Anything else?").await;
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let question =
        crate::daemon::test_support::ask(&manager, &workspace.id, "Anything more?").await;
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(workspace.path.exists());
    manager
        .links
        .set_pr(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Action::Acknowledge,
        )
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());
    let completion = manager
        .spaces
        .completion(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert!(completion.error.is_none());
    // A thread opened after a sweep checked for one still stops its removal.
    manager
        .spaces
        .remove_completed(
            &*manager,
            &Space::from(workspace.clone()),
            &completion,
            EventCause::Completion,
        )
        .await
        .unwrap_err();
    manager
        .spaces
        .remove_merged(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            &completion.head,
        )
        .await
        .unwrap_err();
    assert!(workspace.path.exists());
    assert!(
        manager
            .coordination
            .has_waiting_threads(&workspace.id)
            .await
            .unwrap()
    );
    crate::daemon::test_support::answer(&manager, question.number, "No").await;
    manager.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn completion_cleans_up_commits_the_start_branch_took_unchanged() {
    let (_root, manager, base, workspace) = stacked_fixture().await;
    fs::write(base.path.join("base-work"), "base").unwrap();
    commit(&base.path, "base-work");
    cherry_pick(
        &base.path,
        workspace.branch().map(BranchName::as_str).unwrap(),
    );
    // The idle rule counts the commits as kept as well.
    assert!(
        manager
            .spaces
            .cleanup_snapshot(&*manager, &workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(!workspace.path.exists());
    // Neither a remote nor the default branch has its commits.
    let repo = manager
        .repositories
        .repository(workspace.repository())
        .await
        .unwrap();
    assert_ne!(git(&repo.path, &["branch", "--list", "part"]), "");
}

#[tokio::test]
async fn completion_retains_commits_the_start_branch_took_adapted() {
    let (_root, manager, base, workspace) = stacked_fixture().await;
    fs::write(base.path.join("part-work"), "adapted").unwrap();
    commit(&base.path, "part-work");
    assert_eq!(
        manager
            .spaces
            .cleanup_candidacy(&*manager, &workspace.id)
            .await
            .unwrap(),
        Err(CleanupReason::Unpushed { commits: 1 })
    );
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert_retained(&manager, &workspace).await;
    // Marking the same HEAD integrated updates the completion.
    let completion = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                integrated: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(completion.integrated && completion.error.is_none());
    assert_eq!(
        manager
            .spaces
            .cleanup_candidacy(&*manager, &workspace.id)
            .await
            .unwrap(),
        Err(CleanupReason::Completed { integrated: true })
    );
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(!workspace.path.exists());
    let repo = manager
        .repositories
        .repository(workspace.repository())
        .await
        .unwrap();
    assert_ne!(git(&repo.path, &["branch", "--list", "part"]), "");
}

#[tokio::test]
async fn an_integrated_completion_retains_dirty_and_newer_work() {
    let (_root, manager, workspace) = fixture().await;
    fs::write(workspace.path.join("new-work"), "keep").unwrap();
    commit(&workspace.path, "new-work");
    fs::write(workspace.path.join("unsaved"), "keep").unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                integrated: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert_retained(&manager, &workspace).await;
    // A commit after the mark lapses it like any completion.
    commit(&workspace.path, "unsaved");
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert_retained(&manager, &workspace).await;
}

#[tokio::test]
async fn plain_done_returns_an_integrated_completion_to_normal_retention() {
    let (_root, manager, workspace) = fixture().await;
    fs::write(workspace.path.join("new-work"), "keep").unwrap();
    commit(&workspace.path, "new-work");
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            "app".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                integrated: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    let completion = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(!completion.integrated);
    manager
        .spaces
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            "app".into(),
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert_retained(&manager, &workspace).await;
}

/// A workspace `part` stacked on the workspace `finished`, with one commit
/// of its own.
async fn stacked_fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace, Workspace) {
    let (root, manager, base) = fixture().await;
    let workspace = manager
        .create_workspace(
            base.repository(),
            "part".into(),
            base.branch().map(ToString::to_string),
            None,
            None,
        )
        .await
        .unwrap();
    fs::write(workspace.path.join("part-work"), "part").unwrap();
    commit(&workspace.path, "part-work");
    (root, manager, base, workspace)
}

fn cherry_pick(repo: &std::path::Path, revision: &str) {
    git(
        repo,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "cherry-pick",
            revision,
        ],
    );
}

#[tokio::test]
async fn a_parents_completion_takes_its_unheld_children_whatever_their_work() {
    let (_root, manager, parent) = fixture().await;
    // The parent's own unpushed commit keeps it, but not its children.
    commit_file(&parent.path, "parent-work");
    let split = |name: &'static str, parent: SpaceId| {
        let manager = manager.clone();
        async move {
            manager
                .spaces
                .create_child(
                    &*manager,
                    &SpaceSelector::from(parent.as_str()),
                    name.into(),
                    None,
                    Default::default(),
                )
                .await
                .unwrap()
        }
    };
    let dirty = split("dirty", parent.id.clone()).await;
    commit_file(&dirty.path, "child-work");
    fs::write(dirty.path.join("draft"), "draft").unwrap();
    let held = split("held", parent.id.clone()).await;
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&held.id),
            "review".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let nested = split("nested", parent.id.clone()).await;
    let leaf = split("leaf", nested.id.clone()).await;

    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&parent.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(
        manager
            .spaces
            .cleanup_reason(&*manager, &crate::model::Space::from(dirty.clone()))
            .await,
        CleanupReason::AncestorCompleted {
            workspace: "finished".into()
        }
    );
    assert_eq!(
        manager
            .spaces
            .cleanup_reason(&*manager, &crate::model::Space::from(held.clone()))
            .await,
        CleanupReason::Held
    );
    // The leaf goes first, then the workspace it was split from.
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    for gone in [&dirty, &nested, &leaf] {
        assert!(
            manager
                .spaces
                .find_workspace(&gone.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!gone.path.exists(), "{}", gone.name);
    }
    // Its commit stays on the kept branch, its unsaved file in a checkpoint.
    let repo = manager
        .repositories
        .repository(parent.repository())
        .await
        .unwrap();
    assert_eq!(
        git(&repo.path, &["branch", "--list", "dirty"]).trim(),
        "dirty"
    );
    let removed = manager.spaces.removed_workspaces().await.unwrap();
    assert!(removed.iter().any(|removed| removed.name == "dirty"));
    assert!(held.path.exists());
    assert!(parent.path.exists());
    assert_retained(&manager, &parent).await;
}

#[tokio::test]
async fn a_held_parents_completion_takes_the_chats_below_it_deepest_first() {
    let (_root, manager, parent) = fixture().await;
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "review".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let mut above = parent.id.clone();
    let mut chats = Vec::new();
    for name in ["sub", "nested"] {
        let start = crate::daemon::chats::NewChat {
            priority: None,
            narrowing: Default::default(),
            mode: "chat".into(),
            name: name.into(),
            agent: "claude".into(),
            origin: None,
            parent: Some(above),
        };
        let chat = manager
            .spaces
            .start_new_chat(
                &*manager,
                Some(parent.repository().clone().into()),
                None,
                start,
            )
            .await
            .unwrap();
        let chat = manager.run_chat_agent(chat, None).await.unwrap();
        above = chat.id.clone();
        chats.push(chat);
    }

    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&parent.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // The nested chat goes first, then the chat it was below.
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(
        manager
            .spaces
            .chat_by_id(&chats[1].id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        manager
            .spaces
            .chat_by_id(&chats[0].id)
            .await
            .unwrap()
            .is_some()
    );
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(
        manager
            .spaces
            .chat_by_id(&chats[0].id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        manager
            .spaces
            .find_workspace(&parent.id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn children_stay_while_their_parents_completion_does_not_stand() {
    let (_root, manager, parent) = fixture().await;
    commit_file(&parent.path, "parent-work");
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    // Without cleanup the completion leaves them.
    manager
        .write_local_config(
            parent.repository(),
            Some("[cleanup.done]\nenabled = false\n".into()),
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&parent.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(child.path.exists());
    // Nor once it is withdrawn.
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&parent.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .spaces
        .undo_space_done(&*manager, &SpaceSelector::id(&parent.id))
        .await
        .unwrap();
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(child.path.exists());
    // Nor once the parent committed after it.
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&parent.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    commit_file(&parent.path, "parent-work");
    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(child.path.exists());
    assert!(matches!(
        manager
            .spaces
            .cleanup_reason(&*manager, &crate::model::Space::from(child.clone()))
            .await,
        CleanupReason::Idle { .. }
    ));
}

/// Commit a new revision of the file `name` in `path`.
fn commit_file(path: &std::path::Path, name: &str) {
    let file = path.join(name);
    let revision = fs::read_to_string(&file).unwrap_or_default();
    fs::write(&file, format!("{revision}x")).unwrap();
    commit(path, name);
}

#[tokio::test]
async fn an_unreadable_ancestor_completion_leaves_other_completions_to_their_cleanup() {
    let (_root, manager, parent) = fixture().await;
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let other = manager
        .create_workspace(parent.repository(), "other".into(), None, None, None)
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&other.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let id = parent.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_completion(space_id,record) VALUES (?1,'not json')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let error = manager.spaces.sweep_completed(&*manager).await.unwrap_err();
    assert!(error.to_string().contains("child: "), "{error:#}");
    assert!(!other.path.exists());
    assert!(child.path.exists() && parent.path.exists());
    // The unreadable record stays for diagnosis.
    let id = parent.id.clone();
    let record: String = manager
        .store
        .run(move |db| {
            Ok(db.query_row(
                "SELECT record FROM space_completion WHERE space_id=?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(record, "not json");
}

#[tokio::test]
async fn a_worker_opened_outside_its_done_chat_after_the_sweep_chose_it_stays() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let worker = manager
        .spaces
        .create_workspace_below(
            &*manager,
            repo.id.into(),
            "worker".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&chat.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &crate::daemon::workspace_files::Attacher::User,
        )
        .await
        .unwrap();
    let ancestor = manager
        .spaces
        .completed_ancestor(&*manager, &worker.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ancestor.id, chat.id);

    // The user opens it outside the chat before the removal reserves it.
    manager
        .spaces
        .assign_parent(&*manager, &worker.id, None)
        .await
        .unwrap();
    manager
        .spaces
        .remove_under_completed(&*manager, &Space::from(worker.clone()), &ancestor)
        .await
        .unwrap_err();
    assert!(manager.spaces.workspace_by_id(&worker.id).await.is_ok());
}

#[tokio::test]
async fn a_removal_under_a_done_chat_keeps_the_chats_workspaces_below_it() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let root = manager
        .spaces
        .create_workspace_below(
            &*manager,
            repo.id.into(),
            "root".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    let split = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&root.id),
            "split".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&chat.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &crate::daemon::workspace_files::Attacher::User,
        )
        .await
        .unwrap();
    let split = manager.spaces.workspace_by_id(&split.id).await.unwrap();
    let ancestor = manager
        .spaces
        .completed_ancestor(&*manager, &split.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ancestor.id, chat.id);

    // While a removal under the chat holds its workspaces, the root that
    // puts the split child below the chat stays there...
    let held = manager.spaces.completed_owners.reserve(&chat.id).unwrap();
    manager
        .spaces
        .assign_parent(&*manager, &root.id, None)
        .await
        .unwrap_err();
    // ...and the removal takes that hold itself, so another one waits.
    manager
        .spaces
        .remove_under_completed(&*manager, &Space::from(split.clone()), &ancestor)
        .await
        .unwrap_err();
    assert!(manager.spaces.workspace_by_id(&split.id).await.is_ok());
    drop(held);
    manager
        .spaces
        .assign_parent(&*manager, &root.id, None)
        .await
        .unwrap();
}
