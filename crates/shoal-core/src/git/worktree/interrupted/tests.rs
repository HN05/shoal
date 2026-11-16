use super::*;
use crate::git::worktree::{BranchRemoval, FileRemoval};
use crate::test_support::{commit, git, repository};

/// A repository with a tracked file at its root, one in a directory and an
/// ignored `target/`, and a worktree of it on `topic` at `<root>/topic`
/// with built output there, with the worktree's admin directory.
async fn worktree(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let repo = repository(root, "repo");
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "fn main() {}\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    commit(&repo, "src/lib.rs");
    commit(&repo, ".gitignore");
    let workspace = root.join("topic");
    super::super::add(&repo, &workspace, &"topic".into(), Some("main"))
        .await
        .unwrap();
    std::fs::create_dir(workspace.join("target")).unwrap();
    std::fs::write(workspace.join("target/output"), "built").unwrap();
    let admin = PathBuf::from(git(&workspace, &["rev-parse", "--absolute-git-dir"]).trim_end());
    (repo, workspace, admin)
}

/// Delete what a `worktree remove` stopped partway may have: tracked
/// files, a whole directory of them, ignored output and the `.git` link.
fn delete_partway(workspace: &Path) {
    std::fs::remove_file(workspace.join("tracked")).unwrap();
    std::fs::remove_dir_all(workspace.join("src")).unwrap();
    std::fs::remove_dir_all(workspace.join("target")).unwrap();
    std::fs::remove_file(workspace.join(".git")).unwrap();
}

#[tokio::test]
async fn a_stopped_removal_gets_its_link_and_tracked_files_back() {
    let root = tempfile::tempdir().unwrap();
    let (_repo, workspace, admin) = worktree(root.path()).await;
    let link = std::fs::read_to_string(workspace.join(".git")).unwrap();
    mark(&admin, link).await.unwrap();
    delete_partway(&workspace);
    assert!(restore(&workspace, &admin).await.unwrap());
    assert_eq!(git(&workspace, &["status", "--porcelain"]), "");
    assert!(workspace.join("src/lib.rs").exists());
    assert!(!workspace.join("target").exists());
    // The removal is forgotten once its files are back.
    assert!(!restore(&workspace, &admin).await.unwrap());
}

/// Files deleted while no removal ran are someone's change, which stays.
#[tokio::test]
async fn deletions_without_a_removal_stay() {
    let root = tempfile::tempdir().unwrap();
    let (_repo, workspace, admin) = worktree(root.path()).await;
    std::fs::remove_file(workspace.join("tracked")).unwrap();
    assert!(!restore(&workspace, &admin).await.unwrap());
    assert_eq!(git(&workspace, &["status", "--porcelain"]), " D tracked\n");
}

/// A removal Git refused deleted nothing, so the next decision about the
/// worktree reads its deletions as anyone's.
#[tokio::test]
async fn a_refused_removal_is_forgotten() {
    let root = tempfile::tempdir().unwrap();
    let (repo, workspace, admin) = worktree(root.path()).await;
    std::fs::remove_file(workspace.join("tracked")).unwrap();
    let refused = super::super::remove(
        &repo,
        &workspace,
        FileRemoval::CleanOnly,
        BranchRemoval::Keep,
    )
    .await;
    assert!(refused.is_err());
    assert!(!restore(&workspace, &admin).await.unwrap());
    assert!(!workspace.join("tracked").exists());
}

/// Another checkout at the path is not the worktree whose removal stopped,
/// and its files stay as they are.
#[tokio::test]
async fn another_checkout_at_the_path_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let (repo, workspace, admin) = worktree(root.path()).await;
    let other = root.path().join("other");
    super::super::add(&repo, &other, &"other".into(), Some("main"))
        .await
        .unwrap();
    let link = std::fs::read_to_string(workspace.join(".git")).unwrap();
    mark(&admin, link).await.unwrap();
    std::fs::copy(other.join(".git"), workspace.join(".git")).unwrap();
    std::fs::remove_file(workspace.join("tracked")).unwrap();
    assert!(restore(&workspace, &admin).await.is_err());
    assert!(!workspace.join("tracked").exists());
}

/// A `worktree remove` stopped partway leaves the worktree with its tracked
/// files, its error saying that removing it again continues.
#[tokio::test]
async fn a_stopped_removal_fails_with_the_worktree_whole() {
    let root = tempfile::tempdir().unwrap();
    let (_repo, workspace, admin) = worktree(root.path()).await;
    let link = std::fs::read_to_string(workspace.join(".git")).unwrap();
    let marked = super::super::mark_removal(&workspace, Some(&link))
        .await
        .unwrap();
    delete_partway(&workspace);
    let stopped = anyhow::anyhow!("`git worktree remove` was stopped");
    let error =
        super::super::unfinished_removal(&workspace, marked.as_deref(), true, stopped).await;
    assert!(
        format!("{error:#}").contains("stopped partway"),
        "{error:#}"
    );
    assert_eq!(git(&workspace, &["status", "--porcelain"]), "");
    assert!(!restore(&workspace, &admin).await.unwrap());
}

/// A directory at the path of a worktree Git moved away is not the one
/// whose removal stopped, so it gets no `.git` link.
#[tokio::test]
async fn a_directory_left_where_the_worktree_moved_from_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let (repo, workspace, admin) = worktree(root.path()).await;
    let link = std::fs::read_to_string(workspace.join(".git")).unwrap();
    mark(&admin, link).await.unwrap();
    let moved = root.path().join("moved");
    git(
        &repo,
        &[
            "worktree",
            "move",
            workspace.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    std::fs::create_dir(&workspace).unwrap();
    assert!(restore(&workspace, &admin).await.is_err());
    assert!(!workspace.join(".git").exists());
}

/// A stopped removal that cannot put back what it deleted fails with an
/// error the caller tells apart, leaving its marker for the next removal.
#[tokio::test]
async fn a_stopped_removal_that_cannot_restore_says_so() {
    let root = tempfile::tempdir().unwrap();
    let (_repo, workspace, admin) = worktree(root.path()).await;
    let link = std::fs::read_to_string(workspace.join(".git")).unwrap();
    let marked = super::super::mark_removal(&workspace, Some(&link))
        .await
        .unwrap();
    std::fs::write(admin.join("gitdir"), "/elsewhere/.git\n").unwrap();
    let stopped = anyhow::anyhow!("`git worktree remove` was stopped");
    let error =
        super::super::unfinished_removal(&workspace, marked.as_deref(), true, stopped).await;
    assert!(error.downcast_ref::<Unrestored>().is_some(), "{error:#}");
    assert!(began(&admin).await.unwrap());
}
