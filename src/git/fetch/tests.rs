use super::*;
use crate::{
    git::run_isolated,
    test_support::{git, repository},
};
use std::fs;

#[tokio::test]
async fn cleans_up_fetch_resolution_and_operation_failures() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    // A tree can be fetched into a private ref, but cannot resolve to a commit.
    git(&repo, &["update-ref", "refs/test/tree", "HEAD^{tree}"]);
    for failure in ["fetch", "resolve", "operation"] {
        let mut reference = String::new();
        let error = with_temporary_ref::<()>(&repo, "test", run_isolated, async |fetched| {
            reference = fetched.to_owned();
            let source = match failure {
                "fetch" => "refs/heads/missing",
                "resolve" => "refs/test/tree",
                _ => "refs/heads/main",
            };
            fetch_commit(&repo, ".", source, fetched, run_isolated).await?;
            anyhow::bail!("operation failed")
        })
        .await
        .unwrap_err();
        let error = error.to_string();
        match failure {
            "fetch" => assert!(error.contains("couldn't find remote ref"), "{error}"),
            "resolve" => assert!(error.contains("expected commit type"), "{error}"),
            _ => assert_eq!(error, "operation failed"),
        }
        assert!(!reference.is_empty());
        assert_eq!(git(&repo, &["for-each-ref", "refs/shoal/"]), "");
    }
}

#[tokio::test]
async fn cleanup_failure_preserves_operation_error_and_reports_success_failure() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    for operation_fails in [false, true] {
        let mut lock = None;
        let mut reference = String::new();
        let result = with_temporary_ref(&repo, "test", run_isolated, async |fetched| {
            fetch_commit(&repo, ".", "HEAD", fetched, run_isolated).await?;
            let lock_path = repo.join(".git").join(format!("{fetched}.lock"));
            fs::write(&lock_path, "locked").unwrap();
            lock = Some(lock_path);
            reference = fetched.to_owned();
            anyhow::ensure!(!operation_fails, "operation failed");
            Ok(())
        })
        .await;
        let error = result.unwrap_err().to_string();
        assert_eq!(
            error,
            if operation_fails {
                "operation failed"
            } else {
                "could not remove temporary fetch ref"
            }
        );
        fs::remove_file(lock.unwrap()).unwrap();
        git(&repo, &["update-ref", "-d", &reference]);
    }
}

#[tokio::test]
async fn remote_fetch_deletes_tracking_refs_of_deleted_branches() {
    let root = tempfile::tempdir().unwrap();
    let origin = repository(root.path(), "origin");
    git(&origin, &["branch", "gone"]);
    git(&origin, &["branch", "kept"]);
    let clone = root.path().join("clone");
    git(
        root.path(),
        &["clone", "--quiet", origin.to_str().unwrap(), "clone"],
    );
    git(&origin, &["branch", "-D", "gone"]);
    remote(&clone, "origin").await.unwrap();
    let refs = git(
        &clone,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/remotes/origin/",
        ],
    );
    assert!(refs.contains("refs/remotes/origin/kept"), "{refs}");
    assert!(!refs.contains("refs/remotes/origin/gone"), "{refs}");
}

#[tokio::test]
async fn overlapping_operations_keep_distinct_refs_until_consumed() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    let barrier = tokio::sync::Barrier::new(2);
    let fetch_head = repo.join(".git/FETCH_HEAD");
    fs::write(&fetch_head, "sentinel\n").unwrap();
    let operation = async || {
        with_temporary_ref(&repo, "test", run_isolated, async |fetched| {
            let commit = fetch_commit(&repo, ".", "HEAD", fetched, run_isolated).await?;
            barrier.wait().await;
            let refs = git(
                &repo,
                &["for-each-ref", "--format=%(refname)", "refs/shoal/"],
            );
            assert_eq!(refs.lines().count(), 2);
            assert!(refs.lines().any(|reference| reference == fetched));
            assert_eq!(commit, git(&repo, &["rev-parse", "HEAD"]).trim());
            barrier.wait().await;
            Ok(fetched.to_owned())
        })
        .await
        .unwrap()
    };
    let (first, second) = tokio::join!(operation(), operation());
    assert_ne!(first, second);
    assert_eq!(git(&repo, &["for-each-ref", "refs/shoal/"]), "");
    assert_eq!(fs::read_to_string(fetch_head).unwrap(), "sentinel\n");
}
