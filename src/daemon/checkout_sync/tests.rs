use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures_util::FutureExt as _;

use crate::{
    daemon::{notifications::NotificationKind, workspace::Manager},
    test_support::{commit, git, manager, repository},
};

/// A registered checkout whose `main` tracks `origin`, and a clone that
/// pushes to it.
struct Fixture {
    root: tempfile::TempDir,
    manager: Arc<Manager>,
    repo: PathBuf,
    repo_id: String,
    author: PathBuf,
}

/// Register a checkout named `name` under `root` whose `main` tracks its
/// own `origin`; returns it, its ID and a clone that pushes to the origin.
async fn checkout(root: &Path, manager: &Manager, name: &str) -> (PathBuf, String, PathBuf) {
    let repo = repository(root, name);
    let origin = root.join(format!("{name}-origin.git"));
    let origin = origin.to_str().unwrap();
    git(root, &["clone", "--bare", repo.to_str().unwrap(), origin]);
    git(&repo, &["remote", "add", "origin", origin]);
    git(&repo, &["fetch", "origin"]);
    git(&repo, &["branch", "--set-upstream-to=origin/main", "main"]);
    let author = root.join(format!("{name}-author"));
    git(root, &["clone", origin, author.to_str().unwrap()]);
    let repo_id = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
        .id;
    (repo, repo_id, author)
}

/// Push a new commit to the origin `author` cloned; returns it.
fn push(author: &Path, contents: &str) -> String {
    fs::write(author.join("upstream"), contents).unwrap();
    commit(author, "upstream");
    git(author, &["push", "origin", "main"]);
    git(author, &["rev-parse", "HEAD"])
}

impl Fixture {
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let (repo, repo_id, author) = checkout(root.path(), &manager, "repo").await;
        Self {
            root,
            manager,
            repo,
            repo_id,
            author,
        }
    }

    /// Push a new commit to `origin/main`; returns it.
    fn push(&self, contents: &str) -> String {
        push(&self.author, contents)
    }

    fn break_remote(&self) {
        let missing = self.root.path().join("missing.git");
        git(
            &self.repo,
            &["remote", "set-url", "origin", missing.to_str().unwrap()],
        );
    }
}

#[tokio::test]
async fn sync_fast_forwards_main_and_a_recent_fetch_stays_fresh() {
    let f = Fixture::new().await;
    let pushed = f.push("remote\n");
    let synced = f
        .manager
        .sync_repository(&f.repo_id.clone().into())
        .await
        .unwrap();
    assert!(synced.default_branch.updated);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), pushed);
    f.break_remote();
    assert!(f.manager.default_branch_fresh(&f.repo_id).await);
    assert!(f.manager.fetch_default_branch(&f.repo_id).await.is_err());
}

#[tokio::test]
async fn main_that_cannot_follow_its_remote_still_counts_as_fetched() {
    let f = Fixture::new().await;
    let pushed = f.push("remote\n");
    fs::write(f.repo.join("tracked"), "local commit\n").unwrap();
    commit(&f.repo, "tracked");
    let local = git(&f.repo, &["rev-parse", "main"]);
    let error = f
        .manager
        .sync_repository(&f.repo_id.clone().into())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("diverged"), "{error:#}");
    f.manager.fetch_default_branch(&f.repo_id).await.unwrap();
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), local);
    assert_eq!(git(&f.repo, &["rev-parse", "origin/main"]), pushed);
}

#[tokio::test]
async fn an_unfetched_repository_is_not_fresh() {
    let f = Fixture::new().await;
    f.break_remote();
    assert!(!f.manager.default_branch_fresh(&f.repo_id).await);
}

#[tokio::test]
async fn a_request_wakes_the_loop_for_that_repository() {
    let f = Fixture::new().await;
    f.manager.syncs.request(&f.repo_id);
    assert!(f.manager.syncs.wake.notified().now_or_never().is_some());
    assert_eq!(f.manager.syncs.take_requested(), vec![f.repo_id.clone()]);
    assert!(f.manager.syncs.take_requested().is_empty());
}

#[tokio::test]
async fn a_lasting_failure_shows_in_status_then_the_inbox_and_one_notification() {
    let f = Fixture::new().await;
    assert_eq!(f.manager.checkout_sync(&f.repo_id), None);
    f.manager
        .sync_repository(&f.repo_id.clone().into())
        .await
        .unwrap();
    f.break_remote();
    f.manager
        .sync_repository(&f.repo_id.clone().into())
        .await
        .unwrap_err();
    let sync = f.manager.checkout_sync(&f.repo_id).unwrap();
    assert!(sync.synced_at.is_some());
    assert!(sync.failing_since.is_some());
    assert!(sync.error.unwrap().contains("could not fetch origin"));
    assert!(f.manager.failing_syncs().await.unwrap().is_empty());

    // Only the threshold is waited out; no Git runs while the clock is paused.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(30 * 60)).await;
    let failing = f.manager.failing_syncs().await.unwrap();
    assert_eq!(failing.len(), 1);
    assert_eq!(failing[0].repository_id, f.repo_id);
    f.manager.notify_failing_syncs().await;
    f.manager.notify_failing_syncs().await;
    let notifications = f.manager.notifications(false, 100).await.unwrap();
    let told: Vec<_> = notifications
        .iter()
        .filter(|n| n.kind == NotificationKind::SyncFailed)
        .collect();
    assert_eq!(told.len(), 1, "{notifications:?}");
    assert!(told[0].message.contains("retry with shoal repo sync"));
}

#[tokio::test]
async fn a_repository_syncs_while_another_ones_git_gate_is_held() {
    let f = Fixture::new().await;
    let (other, other_id, other_author) = checkout(f.root.path(), &f.manager, "other").await;
    let pushed = push(&other_author, "remote\n");
    let _held = f.manager.gates.lock_git(&f.repo_id).await;
    let syncing = tokio::spawn(super::run(f.manager.clone()));
    // A hang guard only: the other repository's sync never needs the held gate.
    tokio::time::timeout(Duration::from_secs(120), async {
        while f.manager.checkout_sync(&other_id).is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the other repository never synced");
    syncing.abort();
    let sync = f.manager.checkout_sync(&other_id);
    assert_eq!(git(&other, &["rev-parse", "main"]), pushed, "{sync:?}");
    assert_eq!(f.manager.checkout_sync(&f.repo_id), None);
}

#[tokio::test]
async fn workspace_creation_never_waits_on_a_sync_fetching() {
    let f = Fixture::new().await;
    let pushed = f.push("remote\n");
    let fetching = f.manager.gates.lock_fetch(&f.repo_id).await;
    let manager = f.manager.clone();
    let repo_id = f.repo_id.clone();
    let sync = tokio::spawn(async move { manager.sync_repository(&repo_id.into()).await });
    // A hang guard only: creation takes no gate the held fetch holds.
    let workspace = tokio::time::timeout(
        Duration::from_secs(120),
        f.manager
            .create_workspace(&f.repo_id, "worker".into(), None, None, None),
    )
    .await
    .expect("creation waited on the fetch")
    .unwrap();
    assert_ne!(git(&workspace.path, &["rev-parse", "HEAD"]), pushed);
    drop(fetching);
    sync.await.unwrap().unwrap();
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), pushed);
}
