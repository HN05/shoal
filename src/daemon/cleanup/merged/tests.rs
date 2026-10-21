use std::{fs, path::Path, sync::Arc};

use super::*;
use crate::test_support::{commit, git, manager, repository};

const HOUR: u64 = 3600;

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
                    "INSERT INTO workspace_issue(workspace_id,url) VALUES (?1,'issue')",
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

    /// Run a pass `hours` from now and report whether the workspace remains.
    async fn kept_after(&self, hours: u64) -> bool {
        let mut state = State::default();
        sweep_at(&self.manager, &mut state, unix_seconds() + hours * HOUR)
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
    assert!(notifications.iter().any(|notification| {
        notification.kind == NotificationKind::WorkspaceRemoved
            && notification.message.contains("merged cleanup")
    }));
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
        "  topic\n",
        "a branch with unsaved work is kept"
    );
}

#[tokio::test]
async fn holds_questions_running_work_and_unsaved_state_retain_a_merged_workspace() {
    let f = Fixture::new().await;
    f.squash_merge();
    let id = &f.workspace.id;
    let manager = &f.manager;

    manager
        .acquire_hold(id, "review".into(), None)
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    manager.release_hold(id, "review".into()).await.unwrap();

    let ask = crate::daemon::questions::Ask {
        question: "Anything else?".into(),
        recommendation: "No".into(),
        reason: None,
        choices: vec![],
    };
    let question = manager.ask_question(id, ask).await.unwrap();
    assert!(f.kept_after(5).await, "an open question");
    manager
        .answer_question(question.id, "No".into(), None)
        .await
        .unwrap();

    let repo_path = f.checkout.to_str().unwrap().to_owned();
    let stacked = manager
        .create_workspace(repo_path.as_str(), "stacked".into(), None, None, None)
        .await
        .unwrap();
    manager
        .acquire_hold(&stacked.id, "busy".into(), None)
        .await
        .unwrap();
    manager
        .set_base_workspace(&stacked.id, Some("topic".into()))
        .await
        .unwrap();
    assert!(f.kept_after(5).await, "a base of another workspace");
    manager.set_base_workspace(&stacked.id, None).await.unwrap();

    let git_dir = f.workspace.git_dir.clone().unwrap();
    fs::write(git_dir.join("MERGE_HEAD"), "").unwrap();
    assert!(f.kept_after(5).await, "a merge under way");
    fs::remove_file(git_dir.join("MERGE_HEAD")).unwrap();

    let repo = f.workspace.repository_id.clone();
    fs::write(f.workspace.path.join("two"), "edited\n").unwrap();
    manager
        .set_repository_config(
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
        .set_repository_config(&repo, Some("[cleanup.merged]\nenabled = false\n".into()))
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    manager
        .set_repository_config(&repo, Some("[cleanup.merged]\nidle_minutes = 600\n".into()))
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    assert!(!f.kept_after(11).await);
}

#[test]
fn last_change_covers_nested_files_and_worktree_git_state() {
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("tree");
    let git_dir = root.path().join("git");
    fs::create_dir_all(tree.join("a/b")).unwrap();
    fs::create_dir_all(git_dir.join("logs")).unwrap();
    fs::write(tree.join("a/b/file"), "").unwrap();
    let newest = |path: &Path| {
        let metadata = fs::symlink_metadata(path).unwrap();
        metadata.ctime().max(metadata.mtime()) as u64
    };
    let before = last_change(&tree, Some(&git_dir)).unwrap();
    assert!(before >= newest(&tree.join("a/b/file")));
    fs::write(git_dir.join("HEAD"), "").unwrap();
    assert!(last_change(&tree, Some(&git_dir)).unwrap() >= newest(&git_dir.join("HEAD")));
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
    let snapshot = Snapshot {
        rule: super::super::Rule::Merged,
        signals: 1,
        tree: 2,
    };
    let mut state = State::default();
    assert!(!state.waits("w", snapshot));
    state
        .retained
        .insert("w".into(), (snapshot, Instant::now()));
    assert!(state.waits("w", snapshot));
    assert!(!state.waits(
        "w",
        Snapshot {
            tree: 3,
            ..snapshot
        }
    ));
    tokio::time::advance(RETRY_AFTER).await;
    assert!(!state.waits("w", snapshot));
}
