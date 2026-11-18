use std::path::Path;

use super::read;
use crate::test_support::{git, reftable_repository, repository};

/// Which way [`read`] read the repository at `path`.
async fn reader(path: &Path) -> &'static str {
    read(path, |_| Ok("gix"), async |_| Ok("git"))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_repository_with_its_refs_in_files_reads_in_process() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["worktree", "add", "-b", "topic", "../topic"]);
    std::fs::create_dir(repo.join("nested")).unwrap();
    for path in [repo.clone(), repo.join("nested"), root.path().join("topic")] {
        assert_eq!(reader(&path).await, "gix", "{}", path.display());
    }
}

#[tokio::test]
async fn a_reftable_repository_reads_through_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = reftable_repository(root.path(), "repo");
    assert_eq!(reader(&repo).await, "git");
}

#[tokio::test]
async fn an_extension_gix_does_not_know_reads_through_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["config", "core.repositoryFormatVersion", "1"]);
    git(&repo, &["config", "extensions.worktreeConfig", "true"]);
    assert_eq!(reader(&repo).await, "gix");
    git(&repo, &["config", "extensions.someFutureFormat", "true"]);
    assert_eq!(reader(&repo).await, "git");
}

#[tokio::test]
async fn a_partial_clone_reads_through_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["config", "core.repositoryFormatVersion", "1"]);
    git(&repo, &["config", "extensions.partialClone", "origin"]);
    assert_eq!(reader(&repo).await, "git");
}

#[tokio::test]
async fn a_promisor_remote_without_the_extension_reads_through_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(
        &repo,
        &["config", "remote.origin.url", "https://example.com/app"],
    );
    git(&repo, &["config", "remote.origin.promisor", "false"]);
    assert_eq!(reader(&repo).await, "gix");
    git(&repo, &["config", "remote.origin.promisor", "true"]);
    assert_eq!(reader(&repo).await, "git");
}

#[tokio::test]
async fn a_directory_outside_any_repository_reads_through_git() {
    let outside = tempfile::tempdir().unwrap();
    assert_eq!(reader(outside.path()).await, "git");
    assert_eq!(reader(&outside.path().join("missing")).await, "git");
}

/// The variable that names the repository [`a_namespaced_read_helper`]
/// reads.
const HELPER_REPOSITORY: &str = "SHOAL_TEST_NAMESPACED_REPOSITORY";

#[test]
fn a_ref_namespace_leaves_reads_as_git_reads_them() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let mut helper = std::process::Command::new(std::env::current_exe().unwrap());
    helper
        .args([
            "--exact",
            "git::reader::tests::a_namespaced_read_helper",
            "--ignored",
        ])
        .env("GIT_NAMESPACE", "elsewhere")
        .env(HELPER_REPOSITORY, &repo);
    let output = helper.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        crate::test_support::printed(&output)
    );
}

/// Read refs Git sees whatever `GIT_NAMESPACE` names, in a process whose
/// environment sets it.
#[tokio::test]
#[ignore = "run by a_ref_namespace_leaves_reads_as_git_reads_them"]
async fn a_namespaced_read_helper() {
    let repo = std::path::PathBuf::from(std::env::var_os(HELPER_REPOSITORY).unwrap());
    assert!(
        crate::git::ref_exists(&repo, "refs/heads/main")
            .await
            .unwrap()
    );
    assert_eq!(
        crate::git::head_and_branch(&repo).await.unwrap().1,
        Some("main".into())
    );
}
