//! Private fetch refs shared by branch refreshes and tracked merges.
use std::path::Path;

use anyhow::{Context, Result};
use uuid::Uuid;

use super::{FETCH_SAFE_ARGS, resolve_commit};

#[derive(Clone, Copy)]
pub enum FetchPolicy {
    /// Allow Git to update tracking refs selected by the remote's configuration.
    ConfiguredRefmap,
    /// Update only the private destination, regardless of configured refspecs.
    PrivateOnly,
}

/// Keep a unique ref alive for the whole operation, then delete it even when
/// fetching, resolving, or consuming it fails. The operation's error wins over
/// a cleanup error. As with subprocess cancellation, dropping this future does
/// not run asynchronous cleanup.
pub async fn with_temporary_ref<T>(
    repo: &Path,
    namespace: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
    operation: impl AsyncFnOnce(&str) -> Result<T>,
) -> Result<T> {
    let reference = format!("refs/shoal/{namespace}/{}", Uuid::new_v4());
    let result = operation(&reference).await;
    let cleanup = run(repo, &["update-ref", "-d", &reference]).await;
    let value = result?;
    cleanup.context("could not remove temporary fetch ref")?;
    Ok(value)
}

/// Fetch and resolve a commit without reading or writing shared FETCH_HEAD.
/// The caller supplies a ref owned by `with_temporary_ref` and its Git runner.
pub async fn fetch_commit(
    repo: &Path,
    remote: &str,
    source: &str,
    destination: &str,
    policy: FetchPolicy,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    let refspec = format!("{source}:{destination}");
    let mut args = FETCH_SAFE_ARGS.to_vec();
    if matches!(policy, FetchPolicy::PrivateOnly) {
        args.push("--refmap=");
    }
    args.extend(["--", remote, &refspec]);
    run(repo, &args).await?;
    resolve_commit(repo, destination, run).await
}

#[cfg(test)]
mod tests {
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
                fetch_commit(
                    &repo,
                    ".",
                    source,
                    fetched,
                    FetchPolicy::PrivateOnly,
                    run_isolated,
                )
                .await?;
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
                fetch_commit(
                    &repo,
                    ".",
                    "HEAD",
                    fetched,
                    FetchPolicy::PrivateOnly,
                    run_isolated,
                )
                .await?;
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
    async fn overlapping_operations_keep_distinct_refs_until_consumed() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path(), "repo");
        let barrier = tokio::sync::Barrier::new(2);
        let fetch_head = repo.join(".git/FETCH_HEAD");
        fs::write(&fetch_head, "sentinel\n").unwrap();
        let operation = async || {
            with_temporary_ref(&repo, "test", run_isolated, async |fetched| {
                let commit = fetch_commit(
                    &repo,
                    ".",
                    "HEAD",
                    fetched,
                    FetchPolicy::ConfiguredRefmap,
                    run_isolated,
                )
                .await?;
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
}
