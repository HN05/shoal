use crate::execution::recovery::RecordExt;
use std::{fs, sync::Arc};

use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::test_support::{commit, git, manager, repository};
use crate::{
    daemon::{notifications::NotificationKind, workspace_files::Attacher},
    execution::recovery,
    model::CleanupReason,
};

const HOUR: Duration = Duration::from_secs(3600);

struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    checkout: PathBuf,
    workspace: Workspace,
}

impl Fixture {
    /// A workspace with two commits and a linked issue, which keeps it from
    /// idle cleanup.
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "topic".into(), None, None, None)
            .await
            .unwrap();
        for file in ["one", "two"] {
            fs::write(workspace.path.join(file), format!("{file}\n")).unwrap();
            commit(&workspace.path, file);
        }
        let id = workspace.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO space_issue(space_id,url) VALUES (?1,'issue')",
                    [id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        Self {
            _root: root,
            manager,
            checkout,
            workspace,
        }
    }

    /// Squash the branch onto the default branch, as a forge merge would.
    fn squash_merge(&self) {
        git(&self.checkout, &["merge", "-q", "--squash", "topic"]);
        git(&self.checkout, &["commit", "-q", "-m", "squash"]);
    }

    async fn unlink_issue(&self) {
        let id = self.workspace.id.clone();
        self.manager
            .store
            .run(move |db| {
                db.execute("DELETE FROM space_issue WHERE space_id=?1", [id])?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// Let recorded activity no longer keep the workspace, so only its files do.
    async fn age_out_activity(&self) {
        let id = self.workspace.id.clone();
        self.manager
            .store
            .run(move |db| {
                db.execute("UPDATE workspaces SET active_at=0 WHERE id=?1", [id])?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// Run a pass `hours` from now and report whether the workspace remains.
    async fn kept_after(&self, hours: u32) -> bool {
        let mut state = State::default();
        sweep_at(
            &self.manager,
            &mut state,
            UnixTime::now() + hours * HOUR,
            &Default::default(),
        )
        .await
        .unwrap();
        self.manager
            .list_workspaces()
            .await
            .unwrap()
            .iter()
            .any(|workspace| workspace.id == self.workspace.id)
    }
}

#[tokio::test]
async fn merged_workspace_is_removed_once_idle_whatever_its_assignment() {
    let f = Fixture::new().await;
    assert!(f.kept_after(24).await, "unmerged work stays");
    assert!(
        f.manager
            .cleanup_candidacy(&f.workspace.id)
            .await
            .unwrap()
            .is_err(),
        "idle cleanup keeps a workspace with a linked issue"
    );
    f.squash_merge();
    // Files changed just now, and the default idle period is four hours.
    assert!(f.kept_after(3).await);
    assert!(!f.kept_after(5).await);
    assert!(!f.workspace.path.exists());
    assert_eq!(
        git(&f.checkout, &["branch", "--list", "topic"]),
        "",
        "the merged branch is redundant"
    );
    let notifications = f.manager.notifications(true, 100).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == NotificationKind::SpaceRemoved)
    );
}

#[tokio::test]
async fn a_saved_command_keeps_idle_and_unmerged_workspaces_only() {
    saved_work_keeps_idle_and_unmerged_workspaces_only(
        "saved.command.json",
        r#"{"argv":["cargo","test"]}"#,
    )
    .await;
}

#[tokio::test]
async fn a_saved_agent_keeps_idle_and_unmerged_workspaces_only() {
    saved_work_keeps_idle_and_unmerged_workspaces_only(
        "saved.recovery.json",
        r#"{"agent":"claude","stage":"stopped"}"#,
    )
    .await;
}

/// Save `record` as the workspace's state file `name` and check that only a
/// requested cleanup removes the workspace, merged or not.
async fn saved_work_keeps_idle_and_unmerged_workspaces_only(name: &str, record: &str) {
    let f = Fixture::new().await;
    f.unlink_issue().await;
    let directory = f.manager.paths.workspace_state(&f.workspace.id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(name), record).unwrap();
    assert_eq!(
        f.manager.cleanup_candidacy(&f.workspace.id).await.unwrap(),
        Err(CleanupReason::Recovering)
    );
    assert!(f.kept_after(24).await, "unmerged saved work stays");
    f.squash_merge();
    assert_eq!(
        f.manager.cleanup_candidacy(&f.workspace.id).await.unwrap(),
        Err(CleanupReason::Recovering),
        "idle cleanup still keeps saved work after merging"
    );
    let report = super::super::on_request(
        &f.manager,
        true,
        &super::super::Selection {
            spaces: super::super::Pick::All,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(report.removed, std::slice::from_ref(&f.workspace.name));
    assert!(report.kept.is_empty());
    assert!(!f.kept_after(5).await);
}

#[tokio::test]
async fn merged_cleanup_waits_for_grace_and_keeps_saved_agents_for_restore() {
    let f = Fixture::new().await;
    f.unlink_issue().await;
    f.squash_merge();
    f.manager
        .write_local_config(
            &f.workspace.repository_id,
            Some("[cleanup]\ngrace_minutes = 600\n".into()),
        )
        .await
        .unwrap();
    let path = recovery::record_path(&f.manager.paths, &f.workspace.id, "saved");
    fs::create_dir_all(f.manager.paths.workspace_state(&f.workspace.id)).unwrap();
    let record = r#"{"agent":"claude","stage":"stopped","session":"kept-session","stop_reason":"memory pressure"}"#;
    fs::write(&path, record).unwrap();
    let saved = recovery::Record::load(&path).unwrap().unwrap();
    let queued = f
        .manager
        .queue_stopped_restore(
            &SpaceSelector::id(&f.workspace.id),
            "saved",
            Default::default(),
        )
        .await
        .unwrap();

    assert!(f.kept_after(5).await);
    assert!(
        f.manager
            .cleanup_at(&f.workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(path.exists());
    assert!(
        f.manager
            .queue_entries()
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.id == queued.id)
    );
    let id = f.workspace.id.clone();
    f.manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE cleanup_grace SET cleanup_at=0 WHERE space_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(!f.kept_after(5).await);
    assert!(!path.exists());
    assert!(f.manager.queue_entries().await.unwrap().is_empty());
    let removed = f.manager.removed_workspaces().await.unwrap();
    let removed = removed.first().unwrap();
    assert_eq!(removed.cause, EventCause::Merged);
    assert_eq!(removed.agents.len(), 1);
    let agent = removed.agents.first().unwrap();
    assert_eq!(agent.execution, "saved");
    assert_eq!(agent.record, saved);

    let restored = f.manager.restore_workspace(&"topic".into()).await.unwrap();
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    let workspace = restored.opened.workspace;
    assert_eq!(
        recovery::Record::load(&recovery::record_path(
            &f.manager.paths,
            &workspace.id,
            "saved"
        ))
        .unwrap(),
        Some(saved)
    );
    assert!(f.manager.queue_entries().await.unwrap().is_empty());
}

#[tokio::test]
async fn saved_agents_do_not_hide_active_commands_from_merged_cleanup() {
    let f = Fixture::new().await;
    f.squash_merge();
    let path = recovery::record_path(&f.manager.paths, &f.workspace.id, "saved");
    fs::create_dir_all(f.manager.paths.workspace_state(&f.workspace.id)).unwrap();
    fs::write(path, r#"{"agent":"claude","stage":"stopped"}"#).unwrap();
    let id = f.workspace.id.clone();
    f.manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,workspace_id,state) VALUES ('unknown',?1,'unknown')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    assert!(
        f.manager
            .merged_snapshot(&f.workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(f.manager.cleanup_at(&f.workspace.id).await.unwrap(), None);
}

#[tokio::test]
async fn files_never_committed_keep_a_workspace_without_commits() {
    let f = Fixture::new().await;
    git(&f.workspace.path, &["reset", "-q", "--hard", "main"]);
    fs::write(f.workspace.path.join("draft"), "draft\n").unwrap();
    assert!(f.kept_after(5).await);
    fs::remove_file(f.workspace.path.join("draft")).unwrap();
    assert!(!f.kept_after(5).await, "nothing to lose");
}

#[tokio::test]
async fn unsaved_files_of_a_merged_workspace_go_into_its_checkpoint() {
    let f = Fixture::new().await;
    f.squash_merge();
    let head = git(&f.workspace.path, &["rev-parse", "HEAD"]);
    fs::write(f.workspace.path.join("two"), "edited\n").unwrap();
    fs::write(f.workspace.path.join("scratch"), "notes\n").unwrap();
    assert!(!f.kept_after(5).await);

    let snapshot = git(
        &f.checkout,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/shoal/checkpoints/",
        ],
    );
    let snapshot = snapshot.trim();
    assert_eq!(
        git(&f.checkout, &["rev-parse", &format!("{snapshot}^")]),
        head
    );
    assert_eq!(
        git(&f.checkout, &["show", &format!("{snapshot}:two")]),
        "edited\n"
    );
    assert_eq!(
        git(&f.checkout, &["show", &format!("{snapshot}:scratch")]),
        "notes\n"
    );
    assert_eq!(
        git(&f.checkout, &["branch", "--list", "topic"]),
        "",
        "the checkpoint keeps the unsaved files, so the merged branch goes"
    );
}

#[tokio::test]
async fn holds_questions_running_work_and_unsaved_state_retain_a_merged_workspace() {
    let f = Fixture::new().await;
    f.squash_merge();
    let id = &f.workspace.id;
    let manager = &f.manager;

    manager
        .acquire_hold(
            &SpaceSelector::id(id),
            "review".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    manager
        .release_hold(
            &SpaceSelector::id(id),
            "review".into(),
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();

    let ask = crate::daemon::questions::Ask {
        question: "Anything else?".into(),
        recommendation: "No".into(),
        reason: None,
        choices: vec![],
    };
    let question = manager
        .ask_question(&SpaceSelector::id(id), ask, Vec::new(), &Attacher::User)
        .await
        .unwrap();
    assert!(f.kept_after(5).await, "an open question");
    manager
        .answer_question(question.number, "No".into(), None)
        .await
        .unwrap();

    let repo_path = f.checkout.to_str().unwrap().to_owned();
    let stacked = manager
        .create_workspace(repo_path.as_str(), "stacked".into(), None, None, None)
        .await
        .unwrap();
    manager
        .acquire_hold(
            &SpaceSelector::id(&stacked.id),
            "busy".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    manager
        .set_base_workspace(&SpaceSelector::id(&stacked.id), Some("topic".into()))
        .await
        .unwrap();
    assert!(f.kept_after(5).await, "a base of another workspace");
    manager
        .set_base_workspace(&SpaceSelector::id(&stacked.id), None)
        .await
        .unwrap();

    let git_dir = f.workspace.git_dir.clone().unwrap();
    fs::write(git_dir.join("MERGE_HEAD"), "").unwrap();
    assert!(f.kept_after(5).await, "a merge under way");
    fs::remove_file(git_dir.join("MERGE_HEAD")).unwrap();

    let repo = f.workspace.repository_id.clone();
    fs::write(f.workspace.path.join("two"), "edited\n").unwrap();
    manager
        .write_local_config(
            &repo,
            Some("[cleanup.restore]\narchive_max_mb = 0\n".into()),
        )
        .await
        .unwrap();
    assert!(
        f.kept_after(5).await,
        "unsaved files beyond the archive limit"
    );
    git(&f.workspace.path, &["checkout", "--", "two"]);

    manager
        .write_local_config(&repo, Some("[cleanup.merged]\nenabled = false\n".into()))
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    manager
        .write_local_config(&repo, Some("[cleanup.merged]\nidle_minutes = 600\n".into()))
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    assert!(!f.kept_after(11).await);
}

#[test]
fn a_walk_sees_changes_of_nested_files_and_worktree_git_state() {
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("tree");
    let git_dir = root.path().join("git");
    fs::create_dir_all(tree.join("a/b")).unwrap();
    fs::create_dir_all(git_dir.join("logs")).unwrap();
    // Future modification times, each newer than every change time the
    // fixture's own writes leave, so only reading that file reports it.
    let now = UnixTime::now();
    let changes = [
        (tree.join("a/b/file"), now + Duration::from_secs(1000)),
        (git_dir.join("HEAD"), now + Duration::from_secs(2000)),
        (git_dir.join("index"), now + Duration::from_secs(3000)),
        (git_dir.join("logs/HEAD"), now + Duration::from_secs(4000)),
    ];
    for (path, modified) in changes {
        fs::write(&path, "").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified.system_time())
            .unwrap();
        assert_eq!(
            walk_with_git_state(&tree, Some(&git_dir)).unwrap().changed,
            modified,
            "{}",
            path.display()
        );
    }
}

#[tokio::test]
async fn a_failed_fetch_keeps_merged_workspaces() {
    let f = Fixture::new().await;
    f.squash_merge();
    let missing = f.checkout.with_file_name("missing-origin");
    git(
        &f.checkout,
        &["remote", "add", "origin", missing.to_str().unwrap()],
    );
    git(
        &f.checkout,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    git(
        &f.checkout,
        &["branch", "--set-upstream-to=origin/main", "main"],
    );
    assert!(f.kept_after(5).await, "stale refs decide nothing");
    git(&f.checkout, &["remote", "remove", "origin"]);
    assert!(!f.kept_after(5).await);
}

#[tokio::test(start_paused = true)]
async fn a_failed_removal_is_retried_after_a_while_or_once_changed() {
    let mut state = State::default();
    assert!(!state.waits("w", 1));
    state.retained.insert("w".into(), (1, Instant::now()));
    assert!(state.waits("w", 1));
    assert!(!state.waits("w", 2), "HEAD or activity changed");
    tokio::time::advance(RETRY_AFTER).await;
    assert!(!state.waits("w", 1));
}

#[tokio::test]
async fn a_recent_file_change_defers_walks_until_its_idle_period_passes() {
    let f = Fixture::new().await;
    f.squash_merge();
    f.age_out_activity().await;
    let mut state = State::default();
    let now = UnixTime::now();
    sweep_at(&f.manager, &mut state, now, &Default::default())
        .await
        .unwrap();
    let changed = state.changed[&f.workspace.id];
    assert!(changed + 4 * HOUR > now, "files changed just now");
    assert!(state.changed_after(&f.workspace.id, changed - Duration::from_secs(1)));
    assert!(!state.changed_after(&f.workspace.id, changed));
    sweep_at(&f.manager, &mut state, now + 5 * HOUR, &Default::default())
        .await
        .unwrap();
    assert!(!f.workspace.path.exists());
    sweep_at(&f.manager, &mut state, now + 5 * HOUR, &Default::default())
        .await
        .unwrap();
    assert!(state.changed.is_empty(), "a removed workspace is forgotten");
}

#[tokio::test]
async fn a_future_modification_time_defers_walks_no_later_than_the_walk() {
    let f = Fixture::new().await;
    f.squash_merge();
    f.age_out_activity().await;
    let file = f.workspace.path.join("future");
    fs::write(&file, "").unwrap();
    let now = UnixTime::now();
    let future = std::time::SystemTime::now() + 1000 * HOUR;
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(future)
        .unwrap();
    let mut state = State::default();
    sweep_at(&f.manager, &mut state, now, &Default::default())
        .await
        .unwrap();
    assert_eq!(state.changed[&f.workspace.id], now);
}

#[tokio::test]
async fn merged_cleanup_removes_a_chat_stopped_for_its_idle_period() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    manager
        .start_chat(
            repo.id.into(),
            "talk".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let mut state = State::default();
    let now = UnixTime::now();
    // Running, it is in use.
    sweep_at(&manager, &mut state, now + 300 * HOUR, &Default::default())
        .await
        .unwrap();
    assert_eq!(manager.chats().await.unwrap().len(), 1);
    process.kill();
    manager.stop_finished_chats().await.unwrap();
    // Stopped just now, it is not idle yet.
    sweep_at(&manager, &mut state, now, &Default::default())
        .await
        .unwrap();
    assert_eq!(manager.chats().await.unwrap().len(), 1);
    sweep_at(&manager, &mut state, now + 5 * HOUR, &Default::default())
        .await
        .unwrap();
    assert!(manager.chats().await.unwrap().is_empty());
}
