//! Private fetch refs for branch refreshes.
use std::path::Path;

use anyhow::{Context, Result};
use uuid::Uuid;

use super::{FETCH_SAFE_ARGS, resolve_commit};

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

/// Fetch and resolve a commit without reading or writing shared FETCH_HEAD;
/// Git still updates the tracking refs the remote's configuration selects.
/// The caller supplies a ref owned by `with_temporary_ref` and its Git runner.
pub async fn fetch_commit(
    repo: &Path,
    remote: &str,
    source: &str,
    destination: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    let refspec = format!("{source}:{destination}");
    let mut args = FETCH_SAFE_ARGS.to_vec();
    args.extend(["--", remote, &refspec]);
    run(repo, &args).await?;
    resolve_commit(repo, destination, run).await
}

/// Refresh the configured tracking refs of a remote, deleting those whose
/// branch the remote no longer has: a stale ref would count commits as pushed
/// that exist only locally.
pub async fn remote(repo: &Path, remote: &str) -> Result<String> {
    super::run_isolated(
        repo,
        &[FETCH_SAFE_ARGS, &["--prune", "--", remote]].concat(),
    )
    .await
}

/// Refresh a branch using the remote's configured tracking-ref mapping.
pub async fn configured_branch(repo: &Path, remote: &str, branch: &str) -> Result<String> {
    let tracking = super::remote_ref(remote, branch);
    let refspec = format!("+{}:{tracking}", super::local_ref(branch));
    super::run_isolated(repo, &[FETCH_SAFE_ARGS, &["--", remote, &refspec]].concat()).await?;
    Ok(tracking)
}

/// Fetch a branch for a new worktree, preserving unrelated tracking refs.
pub async fn materialize_branch(repo: &Path, remote: &str, branch: &str) -> Result<String> {
    let tracking = super::remote_ref(remote, branch);
    super::run_isolated(
        repo,
        &[
            &["-c", "fetch.prune=false"][..],
            FETCH_SAFE_ARGS,
            &[
                "--refmap=",
                "--",
                remote,
                &format!("+{}:{tracking}", super::local_ref(branch)),
            ],
        ]
        .concat(),
    )
    .await?;
    Ok(tracking)
}

/// Fetch only a review's branch, retaining the caller's normal Git policy.
pub async fn review_branch(repo: &Path, branch: &str) -> Result<String> {
    let tracking = super::remote_ref("origin", branch);
    super::run(
        repo,
        &[
            FETCH_SAFE_ARGS,
            &[
                "--refmap=",
                "--",
                "origin",
                &format!("+{}:{tracking}", super::local_ref(branch)),
            ],
        ]
        .concat(),
    )
    .await?;
    Ok(tracking)
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
}
