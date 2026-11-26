//! The Git commands of `git-remote-shoal`, the push gate's remote helper:
//! Git without the push rewrites, so it reaches the real remote. See
//! `docs/design/permissions.md`.
use std::{ffi::OsStr, io, process::Stdio};

use anyhow::{Context, Result};
use shoal_refs::CommitId;
use tokio::process::Command;

use crate::tools::Tool;

/// The remote's refs at `url`, as `git ls-remote` prints them.
pub async fn list_remote(url: &str) -> Result<String> {
    output(&["ls-remote", "--", url])
        .await
        .context("list the remote's refs")
}

/// The object `rev` names.
pub async fn resolve(rev: &str) -> Result<CommitId> {
    let object = output(&["rev-parse", "--verify", "--end-of-options", rev])
        .await
        .with_context(|| format!("resolve {rev}"))?;
    Ok(object.trim().into())
}

/// Every push URL of `remote`.
pub async fn push_urls(remote: &str) -> Result<Vec<String>> {
    let urls = output(&["remote", "get-url", "--push", "--all", remote]).await?;
    Ok(urls.lines().map(str::to_owned).collect())
}

/// Push `refspecs` to `url` without the hooks Git already ran, and return
/// what `git push --porcelain` printed. `leases` pairs each destination ref
/// with the object `--force-with-lease` expects there. Git's progress and
/// errors reach the user on this process's stderr, and a push has no time
/// limit, since its time grows with what it sends.
pub async fn porcelain_push(
    url: &str,
    refspecs: &[String],
    leases: &[(&str, &str)],
) -> io::Result<String> {
    let output = git(&super::porcelain_push_args(url, refspecs, leases))
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .await?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn output(args: &[&str]) -> Result<String> {
    super::bounded(git(args)).output().await
}

/// Git in the repository Git runs the helper for, which the variables it
/// exports name, without the rewrites.
fn git(args: &[impl AsRef<OsStr>]) -> Command {
    let mut command = Command::new(Tool::Git.program());
    command.args(args);
    crate::agent_git::push_gate::strip(&mut command);
    command
}
