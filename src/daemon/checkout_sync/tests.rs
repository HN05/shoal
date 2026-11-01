use std::{fs, path::PathBuf, sync::Arc};

use futures_util::FutureExt as _;

use crate::{
    daemon::workspace::Manager,
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

impl Fixture {
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo");
        let origin = root.path().join("origin.git");
        let origin = origin.to_str().unwrap();
        git(
            root.path(),
            &["clone", "--bare", repo.to_str().unwrap(), origin],
        );
        git(&repo, &["remote", "add", "origin", origin]);
        git(&repo, &["fetch", "origin"]);
        git(&repo, &["branch", "--set-upstream-to=origin/main", "main"]);
        let author = root.path().join("author");
        git(root.path(), &["clone", origin, author.to_str().unwrap()]);
        let repo_id = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap()
            .id;
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
        fs::write(self.author.join("upstream"), contents).unwrap();
        commit(&self.author, "upstream");
        git(&self.author, &["push", "origin", "main"]);
        git(&self.author, &["rev-parse", "HEAD"])
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
