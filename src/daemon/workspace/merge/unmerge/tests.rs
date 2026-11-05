use std::{fs, path::PathBuf, sync::Arc};

use crate::{
    daemon::{
        approvals::{DecisionStatus, Specification, Undecided},
        scope,
        workspace::{ExecutionKind, ExecutionRequest, Manager, RecordChange},
    },
    model::{MergeOptions, Unmerged, Workspace},
    test_support::{commit, git, manager, repository},
};

struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    manager: Arc<Manager>,
    repo_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo");
        let repo_id = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap()
            .id;
        Self {
            root,
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

    /// Merge the workspace `name` and record it, as a merge execution does.
    async fn merge(&self, name: &str) {
        let _guard = self.manager.gates.lock_git(&self.repo_id).await;
        let plan = self
            .manager
            .prepare_merge(name, &MergeOptions::default())
            .await
            .unwrap();
        crate::git::repo::finish_merge(plan.clone()).await.unwrap();
        let change = RecordChange::Merge(Box::new(plan));
        self.manager.update_merge_records(&change).await;
    }

    /// Undo the workspace `name`'s latest merge and update the records, as
    /// an unmerge execution does, whether or not the worker succeeds.
    async fn unmerge(&self, name: &str) -> anyhow::Result<Unmerged> {
        let _guard = self.manager.gates.lock_git(&self.repo_id).await;
        let plan = self.manager.prepare_unmerge(name).await?;
        let unmerged = crate::git::unmerge::finish_unmerge(plan.clone()).await;
        let change = RecordChange::Unmerge(Box::new(plan));
        self.manager.update_merge_records(&change).await;
        unmerged
    }

    fn main(&self) -> String {
        rev(&self.repo, "main")
    }

    /// Whether `main` holds `commit`.
    fn holds(&self, commit: &str) -> bool {
        git(
            &self.repo,
            &["branch", "--contains", commit, "--list", "main"],
        )
        .contains("main")
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
async fn the_latest_merge_moves_main_back_whether_it_fast_forwarded_or_not() {
    let f = Fixture::new().await;
    let worker = f.add("worker", "feature").await;
    let before = f.main();
    f.merge("worker").await;
    assert_eq!(f.main(), rev(&worker.path, "HEAD"));
    let unmerged = f.unmerge("worker").await.unwrap();
    assert_eq!(
        (unmerged.commit.as_str(), unmerged.replayed),
        (before.as_str(), 0)
    );
    assert_eq!(f.main(), before);
    assert!(!f.repo.join("feature").exists());
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), "");
    assert_eq!(rev(&f.repo, "worker"), rev(&worker.path, "HEAD"));
    assert!(!unmerged.branch_recreated);
    let error = f.unmerge("worker").await.unwrap_err();
    assert!(
        error.to_string().contains("no local merge of worker"),
        "{error:#}"
    );

    // Diverged histories merged with a merge commit.
    write_commit(&f.repo, "tracked", "main side");
    let before = f.main();
    f.merge("worker").await;
    assert_eq!(rev(&f.repo, "main^2"), rev(&worker.path, "HEAD"));
    f.unmerge("worker").await.unwrap();
    assert_eq!(f.main(), before);
    assert!(!f.repo.join("feature").exists());
}

#[tokio::test]
async fn a_removed_workspace_is_restored_before_its_merge_is_undone() {
    let f = Fixture::new().await;
    let worker = f.add("worker", "feature").await;
    let (before, head) = (f.main(), rev(&worker.path, "HEAD"));
    f.merge("worker").await;
    f.manager
        .remove_workspace(
            &worker.id,
            crate::removal::UnsavedWork::All,
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let error = f.manager.unmerge_workspace("worker").await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("restore it with shoal work restore worker"),
        "{error:#}"
    );
    assert_ne!(f.main(), before);

    f.manager.restore_workspace("worker").await.unwrap();
    f.unmerge("worker").await.unwrap();
    assert_eq!(f.main(), before);
    assert_eq!(rev(&f.repo, "worker"), head);
}

#[tokio::test]
async fn an_older_merge_is_taken_out_by_replaying_later_merges() {
    let f = Fixture::new().await;
    let first = f.add("first", "first").await;
    let second = f.add("second", "second").await;
    write_commit(&f.repo, "tracked", "main side");
    let before = f.main();
    f.merge("first").await;
    f.merge("second").await;
    let unmerged = f.unmerge("first").await.unwrap();
    assert_eq!(unmerged.replayed, 1);
    assert!(!f.holds(&rev(&first.path, "HEAD")));
    assert!(f.holds(&rev(&second.path, "HEAD")));
    assert_eq!(rev(&f.repo, "main^1"), before);
    assert_eq!(rev(&f.repo, "main^2"), rev(&second.path, "HEAD"));
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), "");
    // The replayed merge's record follows its new commit, so it can be
    // undone in turn.
    f.unmerge("second").await.unwrap();
    assert_eq!(f.main(), before);
}

#[tokio::test]
async fn unmerge_refuses_stacked_conflicting_and_pushed_merges_changing_nothing() {
    let f = Fixture::new().await;
    f.add("first", "first").await;
    write_commit(&f.repo, "tracked", "main side");
    f.merge("first").await;
    // A workspace stacked on first's commits.
    let stacked = f
        .manager
        .create_workspace(
            &f.repo_id,
            "stacked".into(),
            Some("first".into()),
            None,
            None,
        )
        .await
        .unwrap();
    write_commit(&stacked.path, "stacked", "stacked");
    f.merge("stacked").await;
    let main = f.main();
    let error = f.unmerge("first").await.unwrap_err();
    assert!(
        error.to_string().contains("later merge of stacked")
            && error.to_string().contains("shoal unmerge stacked"),
        "{error:#}"
    );
    assert_eq!(f.main(), main);
    f.unmerge("stacked").await.unwrap();

    // A later commit that changes what the merge brought conflicts.
    write_commit(&f.repo, "first", "changed on main");
    let main = f.main();
    let error = f.unmerge("first").await.unwrap_err();
    assert!(error.to_string().contains("nothing changed"), "{error:#}");
    assert_eq!(f.main(), main);
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), "");
    assert!(!f.repo.join(".git/rebase-merge").exists());

    // Once main went to its upstream, only a revert undoes the merge.
    let origin = f.root.path().join("origin.git");
    git(
        &f.repo,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    git(
        &f.repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&f.repo, &["push", "-q", "-u", "origin", "main"]);
    let error = f.unmerge("first").await.unwrap_err();
    assert!(error.to_string().contains("git revert"), "{error:#}");
    assert_eq!(f.main(), main);
}

#[tokio::test]
async fn unmerge_refuses_to_replay_a_merge_with_changes_of_its_own() {
    let f = Fixture::new().await;
    f.add("first", "first").await;
    let before = f.main();
    f.merge("first").await;
    git(&f.repo, &["switch", "-q", "-c", "side", &before]);
    write_commit(&f.repo, "side", "side");
    git(&f.repo, &["switch", "-q", "main"]);
    git(&f.repo, &["merge", "-q", "--no-ff", "--no-commit", "side"]);
    write_commit(&f.repo, "side", "edited while merging");
    let main = f.main();
    let error = f.unmerge("first").await.unwrap_err();
    assert!(
        error.to_string().contains("changes of its own"),
        "{error:#}"
    );
    assert_eq!(f.main(), main);
}

#[tokio::test]
async fn unmerge_refuses_to_overwrite_an_ignored_file_the_merge_untracked() {
    let f = Fixture::new().await;
    write_commit(&f.repo, "notes", "tracked");
    let first = f.add("first", "first").await;
    git(&first.path, &["rm", "-q", "notes"]);
    write_commit(&first.path, ".gitignore", "notes");
    f.merge("first").await;
    fs::write(f.repo.join("notes"), "kept locally\n").unwrap();
    let main = f.main();
    let error = f.unmerge("first").await.unwrap_err();
    assert!(
        error.to_string().contains("overwrite ignored files")
            && error.to_string().contains("notes"),
        "{error:#}"
    );
    assert_eq!(f.main(), main);
    assert_eq!(
        fs::read_to_string(f.repo.join("notes")).unwrap(),
        "kept locally\n"
    );
}

#[tokio::test]
async fn an_agents_unmerge_waits_for_approval_of_the_merge_it_undoes() {
    let f = Fixture::new().await;
    let worker = f.add("worker", "feature").await;
    let before = f.main();
    f.merge("worker").await;
    let result = f.main();
    let caller = scope::Caller {
        execution: None,
        space_id: worker.id.clone(),
    };
    let begin = async || {
        f.manager
            .begin_execution_placed(
                &worker.id,
                None,
                ExecutionKind::Unmerge,
                Some(&caller),
                ExecutionRequest::default(),
            )
            .await
    };
    let error = begin().await.expect_err("unmerge needs a decision");
    let pending = error
        .downcast::<Undecided>()
        .expect("an approval request")
        .0;
    assert_eq!(pending.status, DecisionStatus::Pending);
    assert!(
        matches!(&pending.specification, Specification::Unmerge(unmerge) if unmerge.result == result)
    );
    assert_eq!(
        pending.subject(),
        format!("undo the merge of worker at {result} ({result})")
    );
    assert_eq!(f.main(), result);
    f.manager
        .decide_approval(pending.number, true, None, Default::default(), None)
        .await
        .unwrap();
    let started = begin().await.unwrap();
    let plan = started.plan.unmerge.as_ref().unwrap();
    assert_eq!(
        (plan.previous.as_str(), plan.tip.as_str()),
        (before.as_str(), result.as_str())
    );
    f.manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Unmerge, Some(1))
        .await
        .unwrap();
}
