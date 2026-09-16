//! Resolve repository policy without assuming a particular branch spelling.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

use crate::worktrunk;

/// Read cached remote HEAD. Creation/pull may discover and cache a missing one;
/// cleanup only reads local metadata and never contacts a remote.
pub async fn resolve(repo: &Path, discover: bool) -> Result<String> {
    let remotes = worktrunk::git(repo, &["remote"]).await?;
    let remotes: Vec<_> = remotes.lines().collect();
    if remotes.is_empty() {
        let head = worktrunk::git(repo, &["symbolic-ref", "--quiet", "HEAD"])
            .await
            .context("local repository has no default branch; select a starting ref with --ref")?;
        return Ok(head
            .trim_end_matches('\n')
            .strip_prefix("refs/heads/")
            .context("repository HEAD does not name a local branch")?
            .to_owned());
    }
    let remote = if remotes.contains(&"origin") {
        "origin"
    } else {
        ensure!(
            remotes.len() == 1,
            "repository default remote is ambiguous (no origin); select a starting ref with --ref"
        );
        remotes[0]
    };
    let head = format!("refs/remotes/{remote}/HEAD");
    let prefix = format!("refs/remotes/{remote}/");
    if let Ok(target) = worktrunk::git(repo, &["symbolic-ref", "--quiet", &head]).await {
        if let Some(branch) = target.trim_end_matches('\n').strip_prefix(&prefix) {
            return Ok(branch.to_owned());
        }
    }
    ensure!(
        discover,
        "repository default branch is unknown; refresh {remote}/HEAD with git remote set-head"
    );
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(["ls-remote", "--symref", "--", remote, "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0");
    let advertised = worktrunk::run(command).await.with_context(|| {
        format!(
            "discover {remote}'s default branch; use --ref to select a starting point explicitly"
        )
    })?;
    let branch = advertised
        .lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        })
        .context(
            "remote HEAD does not advertise a default branch; select a starting ref with --ref",
        )?;
    worktrunk::git(repo, &["symbolic-ref", &head, &format!("{prefix}{branch}")]).await?;
    Ok(branch.to_owned())
}
