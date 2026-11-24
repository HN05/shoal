use crate::stand_in;
use crate::{
    forge::items::action::MergeMethod,
    model::{MergeOptions, Unmerged, Workspace},
};
use shoal_core::test_support::{commit, git, repository};
use shoal_daemon_base::merges::RecordChange;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;
use std::{fs, path::PathBuf};

struct Fixture {
    repo: PathBuf,
    manager: stand_in::Fixture,
    repo_id: RepositoryId,
}

impl Fixture {
    async fn new() -> Self {
        let manager = stand_in::Fixture::new().await;
        let repo = repository(manager.root.path(), "repo");
        let repo_id = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap()
            .id;
        Self {
            repo,
            manager,
            repo_id,
        }
    }

    /// A workspace whose branch has one commit writing `file`.
    async fn add(&self, name: &str, file: &str) -> Workspace {
        let workspace = self
            .manager
            .create_workspace(&self.repo_id, name.into(), None, None, None)
            .await
            .unwrap();
        write_commit(&workspace.path, file, name);
        workspace
    }

    /// Merge the workspace `name` and update the records, as a merge
    /// execution does.
    async fn merge(&self, name: &str) {
        let _guard = self.manager.gates.lock_git(&self.repo_id).await;
        let options = MergeOptions {
            method: Some(MergeMethod::Merge),
            ..MergeOptions::default()
        };
        let selector = self.manager.named(name).await;
        let plan = (self.manager.spaces)
            .prepare_merge(&*self.manager, &selector, &options, None)
            .await
            .unwrap();
        let merged = crate::git::repo::finish_merge(plan.clone()).await;
        let change = RecordChange::Merge(Box::new(plan));
        self.manager.spaces.update_merge_records(&change).await;
        merged.unwrap();
    }

    /// Undo the workspace `name`'s latest merge and update the records, as
    /// an unmerge execution does, whether or not the worker succeeds.
    async fn unmerge(&self, name: &str) -> anyhow::Result<Unmerged> {
        let _guard = self.manager.gates.lock_git(&self.repo_id).await;
        let plan = (self.manager.spaces)
            .prepare_unmerge(&*self.manager, &self.manager.named(name).await)
            .await?;
        let unmerged = crate::git::unmerge::finish_unmerge(plan.clone()).await;
        let change = RecordChange::Unmerge(Box::new(plan));
        self.manager.spaces.update_merge_records(&change).await;
        unmerged
    }

    fn main(&self) -> String {
        rev(&self.repo, "main")
    }
}

fn rev(repo: &std::path::Path, revision: &str) -> String {
    git(repo, &["rev-parse", revision]).trim().to_owned()
}

fn write_commit(path: &std::path::Path, file: &str, text: &str) {
    fs::write(path.join(file), format!("{text}\n")).unwrap();
    commit(path, file);
}

#[tokio::test]
async fn a_removed_workspace_is_restored_before_its_merge_is_undone() {
    let f = Fixture::new().await;
    let worker = f.add("worker", "feature").await;
    let (before, head) = (f.main(), rev(&worker.path, "HEAD"));
    f.merge("worker").await;
    f.manager
        .spaces
        .remove_selected(
            &*f.manager,
            &SpaceSelector::id(&worker.id),
            crate::removal::UnsavedWork::All,
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let error = f
        .manager
        .spaces
        .unmerge_workspace(&SpaceSelector::id(&worker.id))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains(&format!("restore it with shoal restore {}", worker.id)),
        "{error:#}"
    );
    assert_ne!(f.main(), before);

    f.manager
        .restore_workspace(&f.manager.named("worker").await)
        .await
        .unwrap();
    f.unmerge("worker").await.unwrap();
    assert_eq!(f.main(), before);
    assert_eq!(rev(&f.repo, "worker"), head);
}

#[tokio::test]
async fn a_commit_made_after_the_unmerge_was_planned_is_kept() {
    let f = Fixture::new().await;
    f.add("worker", "feature").await;
    f.merge("worker").await;
    let _guard = f.manager.gates.lock_git(&f.repo_id).await;
    let plan = f
        .manager
        .spaces
        .prepare_unmerge(&*f.manager, &f.manager.named("worker").await)
        .await
        .unwrap();
    write_commit(&f.repo, "later", "made meanwhile");
    let main = f.main();
    let error = crate::git::unmerge::finish_unmerge(plan).await.unwrap_err();
    assert!(
        error.to_string().contains("changed during the unmerge"),
        "{error:#}"
    );
    assert_eq!(f.main(), main);
}

#[tokio::test]
async fn a_merge_pushed_after_the_unmerge_was_planned_is_kept() {
    let f = Fixture::new().await;
    let origin = f.manager.root.path().join("origin.git");
    let origin = origin.to_str().unwrap();
    git(&f.repo, &["init", "-q", "--bare", "-b", "main", origin]);
    git(&f.repo, &["remote", "add", "origin", origin]);
    git(&f.repo, &["push", "-q", "-u", "origin", "main"]);
    f.add("worker", "feature").await;
    f.merge("worker").await;
    let _guard = f.manager.gates.lock_git(&f.repo_id).await;
    let plan = f
        .manager
        .spaces
        .prepare_unmerge(&*f.manager, &f.manager.named("worker").await)
        .await
        .unwrap();
    git(&f.repo, &["push", "-q", "origin", "main"]);
    let main = f.main();
    let error = crate::git::unmerge::finish_unmerge(plan).await.unwrap_err();
    assert!(
        error.to_string().contains("upstream got the merge"),
        "{error:#}"
    );
    assert_eq!(f.main(), main);
}

#[tokio::test]
async fn a_default_branch_checked_out_during_the_unmerge_is_left_as_it_is() {
    let f = Fixture::new().await;
    // The remote names main the default branch while it is not checked out.
    let origin = f.manager.root.path().join("origin.git");
    let origin = origin.to_str().unwrap();
    git(&f.repo, &["init", "-q", "--bare", "-b", "main", origin]);
    git(&f.repo, &["remote", "add", "origin", origin]);
    git(&f.repo, &["push", "-q", "-u", "origin", "main"]);
    git(&f.repo, &["remote", "set-head", "origin", "main"]);
    f.add("worker", "feature").await;
    f.merge("worker").await;
    git(&f.repo, &["switch", "-q", "-c", "elsewhere"]);
    let _guard = f.manager.gates.lock_git(&f.repo_id).await;
    let plan = f
        .manager
        .spaces
        .prepare_unmerge(&*f.manager, &f.manager.named("worker").await)
        .await
        .unwrap();
    assert!(plan.checkout.is_none());
    git(&f.repo, &["switch", "-q", "main"]);
    let main = f.main();
    let error = crate::git::unmerge::finish_unmerge(plan).await.unwrap_err();
    assert!(
        error.to_string().contains("checked out during the unmerge"),
        "{error:#}"
    );
    assert_eq!(f.main(), main);
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn other_changes_to_main_after_a_refused_replay_rewrite_no_record() {
    let f = Fixture::new().await;
    f.add("first", "first").await;
    f.add("second", "second").await;
    let before = f.main();
    f.merge("first").await;
    f.merge("second").await;
    let plan = {
        let _guard = f.manager.gates.lock_git(&f.repo_id).await;
        f.manager
            .spaces
            .prepare_unmerge(&*f.manager, &f.manager.named("first").await)
            .await
            .unwrap()
    };
    // The worker refused; main then went back to before both merges and
    // gained one unrelated commit, as long as the history it replaced and
    // with the second merge's author, date and message.
    let second = f.main();
    git(&f.repo, &["reset", "-q", "--hard", &before]);
    fs::write(f.repo.join("unrelated"), "unrelated\n").unwrap();
    git(&f.repo, &["add", "unrelated"]);
    git(&f.repo, &["commit", "-q", "-C", &second]);
    let unrelated = f.main();
    f.manager
        .spaces
        .update_merge_records(&RecordChange::Unmerge(Box::new(plan)))
        .await;
    let error = f.unmerge("second").await.unwrap_err();
    assert!(
        error.to_string().contains("no local merge of second"),
        "{error:#}"
    );
    assert_eq!(f.main(), unrelated);
}
