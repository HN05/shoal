use super::*;
use crate::test_support::{git, reftable_repository, repository};

/// The ways a test repository keeps its refs: in files, which gix reads,
/// and in a reftable, which Git reads for it.
type Create = fn(&Path, &str) -> PathBuf;

/// HEAD reads on a branch, detached and on a branch without commits, in a
/// repository `create` makes.
async fn head_reads(create: Create) {
    let root = tempfile::tempdir().unwrap();
    let repo = create(root.path(), "repo");
    git(&repo, &["switch", "-q", "-c", "feature/x"]);
    let commit: CommitId = git(&repo, &["rev-parse", "HEAD"]).trim().into();
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).trim().to_owned();
    let on_branch = (commit.clone(), Some(BranchName::from("feature/x")));
    assert_eq!(head_and_branch(&repo).await.unwrap(), on_branch);
    assert_eq!(super::super::head_branch(&repo).await.unwrap(), on_branch.1);
    let state = head_state(&repo).await.unwrap();
    assert_eq!((state.commit, state.tree), (commit.clone(), tree));
    git(&repo, &["switch", "-q", "--detach"]);
    assert_eq!(head_and_branch(&repo).await.unwrap(), (commit, None));
    assert_eq!(checked_out_branch(&repo).await.unwrap(), None);
    assert!(super::super::head_branch(&repo).await.is_err());
    git(&repo, &["switch", "-q", "--orphan", "fresh"]);
    let head = head(&repo).await.unwrap();
    assert_eq!(head.branch(), Some("fresh".into()));
    assert!(head.commit().is_err());
}

#[tokio::test]
async fn head_reads_in_a_repository_with_its_refs_in_files() {
    head_reads(repository).await;
}

#[tokio::test]
async fn head_reads_in_a_reftable_repository() {
    head_reads(reftable_repository).await;
}

/// [`checkout_dirs`] of a main checkout and a linked worktree in a
/// repository `create` makes.
async fn checkout_dirs_of_each_worktree(create: Create) {
    let root = tempfile::tempdir().unwrap();
    let repo = create(root.path(), "repo");
    let linked = root.path().join("linked");
    let add = [
        "worktree",
        "add",
        "-q",
        "-b",
        "topic",
        linked.to_str().unwrap(),
    ];
    git(&repo, &add);
    std::fs::create_dir(linked.join("sub")).unwrap();
    let common = std::fs::canonicalize(repo.join(".git")).unwrap();
    let main = CheckoutDirs {
        root: std::fs::canonicalize(&repo).unwrap(),
        common_dir: common.clone(),
        git_dir: common.clone(),
    };
    assert_eq!(checkout_dirs(&repo).await.unwrap(), main);
    let worktree = CheckoutDirs {
        root: std::fs::canonicalize(&linked).unwrap(),
        common_dir: common.clone(),
        git_dir: common.join("worktrees/linked"),
    };
    assert_eq!(checkout_dirs(&linked.join("sub")).await.unwrap(), worktree);
    assert_eq!(git_dir(&linked).await.unwrap(), worktree.git_dir);
    assert_eq!(common_dir(&linked).await.unwrap(), common);
    assert_eq!(
        worktree_root(&linked.join("sub")).await.unwrap(),
        worktree.root
    );
}

#[tokio::test]
async fn checkout_dirs_tell_a_linked_worktree_from_its_main_checkout() {
    checkout_dirs_of_each_worktree(repository).await;
}

#[tokio::test]
async fn checkout_dirs_in_a_reftable_repository() {
    checkout_dirs_of_each_worktree(reftable_repository).await;
}

#[tokio::test]
async fn a_moved_worktree_is_found_where_it_now_is() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let linked = root.path().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "topic",
            linked.to_str().unwrap(),
        ],
    );
    let moved = root.path().join("moved");
    std::fs::rename(&linked, &moved).unwrap();
    assert!(checkout_dirs(&linked).await.is_err());
    // The admin directory still names the old path; the worktree's own
    // `.git` file leads Git to it all the same.
    let dirs = checkout_dirs(&moved).await.unwrap();
    assert_eq!(dirs.root, std::fs::canonicalize(&moved).unwrap());
    let common = std::fs::canonicalize(repo.join(".git")).unwrap();
    assert_eq!(dirs.git_dir, common.join("worktrees/linked"));
}

#[tokio::test]
async fn head_state_names_the_tree_and_no_branch_when_detached() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["checkout", "-q", "-b", "feature/x"]);
    let commit = revision(&repo, "HEAD", run)
        .await
        .unwrap()
        .trim()
        .to_owned();
    let tree = revision(&repo, "HEAD^{tree}", run)
        .await
        .unwrap()
        .trim()
        .to_owned();
    let on_branch = head_state(&repo).await.unwrap();
    assert_eq!(
        on_branch,
        HeadState {
            commit: commit.clone().into(),
            tree: tree.clone(),
            branch: Some("feature/x".to_owned().into()),
        }
    );
    // A branch named HEAD makes the name HEAD ambiguous, but not HEAD itself.
    git(&repo, &["update-ref", "refs/heads/HEAD", "HEAD"]);
    assert_eq!(head_state(&repo).await.unwrap(), on_branch);
    git(&repo, &["checkout", "-q", "--detach"]);
    assert_eq!(head_state(&repo).await.unwrap().branch, None);
}

#[tokio::test]
async fn a_rebased_branchs_log_leaves_out_its_base_and_what_was_seen() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let commit = |message: &str| git(&repo, &["commit", "-q", "--allow-empty", "-m", message]);
    let rev = |name: &str| git(&repo, &["rev-parse", name]).trim().to_owned();
    git(&repo, &["switch", "-q", "-c", "topic"]);
    commit("Topic work");
    let seen = rev("HEAD");
    git(&repo, &["switch", "-q", "main"]);
    commit("Main work");
    git(&repo, &["switch", "-q", "topic"]);
    git(&repo, &["rebase", "-q", "main"]);
    commit("More topic work");
    let (head, base) = (rev("HEAD"), rev("main"));
    let log = log_stat(&repo, &head, &[&seen, &base], 50).await.unwrap();
    assert!(log.contains("More topic work"), "{log}");
    assert!(!log.contains("Main work"), "{log}");
}
