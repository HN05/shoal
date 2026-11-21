use std::{fs, path::PathBuf, sync::Arc};

use super::*;
use crate::{
    daemon::{
        manager::Manager,
        test_support::{commit, configure, git, manager, repository},
    },
    forge::items::action::MergeMethod,
    model::{MergeOptions, Workspace},
};
use shoal_protocol::space_names::SpaceSelector;

/// A workspace of a repository whose `main` tracks `origin`'s, a local
/// repository.
struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    manager: Arc<Manager>,
    workspace: Workspace,
}

impl Fixture {
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo");
        let origin = root.path().join("origin.git");
        let origin = origin.to_str().unwrap();
        git(&repo, &["clone", "--bare", repo.to_str().unwrap(), origin]);
        git(&repo, &["remote", "add", "origin", origin]);
        git(&repo, &["fetch", "origin"]);
        git(&repo, &["branch", "--set-upstream-to=origin/main", "main"]);
        let id = manager
            .repositories
            .register(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap()
            .id;
        let workspace = manager
            .create_workspace(&id, "worker".into(), None, None, None)
            .await
            .unwrap();
        Self {
            root,
            repo,
            manager,
            workspace,
        }
    }

    /// Merge the workspace's branch into `main` as a merge execution does,
    /// pushing with `push`, and return its plan and whether it succeeded.
    async fn merge(&self, push: bool) -> (MergePlan, bool) {
        self.merge_by(push, MergeMethod::Merge).await
    }

    /// [`Self::merge`] by `method`.
    async fn merge_by(&self, push: bool, method: MergeMethod) -> (MergePlan, bool) {
        let selector = SpaceSelector::id(&self.workspace.id);
        let options = MergeOptions {
            push,
            method: Some(method),
            ..MergeOptions::default()
        };
        let plan = self.manager.prepare_merge(&selector, &options, None).await;
        let plan = plan.unwrap();
        let merged = crate::git::repo::finish_merge(plan.clone()).await;
        (plan, merged.is_ok())
    }

    /// Commit to the workspace's branch.
    fn work(&self) {
        fs::write(self.workspace.path.join("feature"), "work\n").unwrap();
        commit(&self.workspace.path, "feature");
    }

    fn main(&self) -> String {
        git(&self.repo, &["rev-parse", "main"]).trim().to_owned()
    }

    /// Push `origin` to the repository `name`, a copy of `origin`'s, and
    /// return its path.
    fn push_to(&self, name: &str) -> String {
        let pushed = self.root.path().join(name);
        let pushed = pushed.to_str().unwrap().to_owned();
        let origin = self.root.path().join("origin.git");
        git(
            &self.repo,
            &["clone", "--bare", origin.to_str().unwrap(), &pushed],
        );
        git(&self.repo, &["config", "remote.origin.pushurl", &pushed]);
        pushed
    }
}

#[tokio::test]
async fn a_push_names_the_tip_it_published_and_the_url_it_pushed_to() {
    let f = Fixture::new().await;
    let pushed_to = f.push_to("push.git");
    f.work();
    let (plan, merged) = f.merge(true).await;
    assert!(merged);
    let pushed = pushed_merge(&plan).await.unwrap().unwrap();
    assert_eq!(
        (
            pushed.remotes,
            pushed.branch.as_str(),
            pushed.commit.as_str()
        ),
        (vec![pushed_to], "main", f.main().as_str())
    );
}

/// Merge by `method` with a push that fails, then again with one that
/// works, which publishes the first merge and merges nothing new.
async fn retried_push_publishes_the_merge_by(method: MergeMethod) {
    let f = Fixture::new().await;
    git(
        &f.repo,
        &["config", "remote.origin.pushurl", "/nonexistent/push.git"],
    );
    f.work();
    // main moves on, so a squash or rebase makes commits of its own.
    fs::write(f.repo.join("other"), "other\n").unwrap();
    commit(&f.repo, "other");
    let (plan, merged) = f.merge_by(true, method).await;
    assert!(!merged);
    assert!(pushed_merge(&plan).await.unwrap().is_none());

    let merge = f.main();
    git(&f.repo, &["config", "--unset", "remote.origin.pushurl"]);
    let (plan, merged) = f.merge_by(true, method).await;
    assert!(merged);
    assert_eq!(f.main(), merge);
    let pushed = pushed_merge(&plan).await.unwrap().unwrap();
    assert_eq!(pushed.commit.as_str(), merge);
}

#[tokio::test]
async fn a_retried_push_publishes_the_merge_a_failed_push_left() {
    retried_push_publishes_the_merge_by(MergeMethod::Merge).await;
}

#[tokio::test]
async fn a_retried_push_publishes_the_squash_a_failed_push_left() {
    retried_push_publishes_the_merge_by(MergeMethod::Squash).await;
}

#[tokio::test]
async fn a_retried_push_publishes_the_rebase_a_failed_push_left() {
    retried_push_publishes_the_merge_by(MergeMethod::Rebase).await;
}

#[tokio::test]
async fn a_merge_that_stays_local_pushed_nothing() {
    let f = Fixture::new().await;
    f.work();
    let (plan, merged) = f.merge(false).await;
    assert!(merged);
    assert!(pushed_merge(&plan).await.unwrap().is_none());
}

#[tokio::test]
async fn a_push_to_a_repository_on_no_forge_follows_no_runs() {
    let f = Fixture::new().await;
    configure(&f.manager, "[pr]\nwatch_merge_runs = true\n").await;
    f.work();
    let (plan, merged) = f.merge(true).await;
    assert!(merged);
    let change = RecordChange::Merge(Box::new(plan));
    f.manager.follow_pushed_merge_runs(&change).await;
    let awaits = f.manager.links.awaits_merge_runs(&f.workspace.id).await;
    assert!(!awaits.unwrap());
}
