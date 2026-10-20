use std::{fs, path::Path, sync::Arc};

use super::*;
use crate::{
    daemon::workspace::Requester,
    test_support::{commit, git, manager, repository},
};

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
async fn keep_choices_running_work_and_unsaved_state_retain_a_merged_workspace() {
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

    manager
        .mark_done(id, Some(false), Requester::Unscoped)
        .await
        .unwrap();
    assert!(f.kept_after(5).await);
    manager.undo_done(id).await.unwrap();

    fs::write(f.workspace.path.join("two"), "edited\n").unwrap();
    assert!(f.kept_after(5).await);
    git(&f.workspace.path, &["checkout", "--", "two"]);

    let repo = f.workspace.repository_id.clone();
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
