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
mod tests;
