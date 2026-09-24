//! Resolve repository policy without assuming a particular branch spelling.
use std::path::Path;

use anyhow::{Context, Result, ensure};

use crate::git;

#[derive(Debug, Clone, Copy)]
pub enum DefaultBranchLookup {
    Cached,
    Discover,
}

/// Read cached remote HEAD. Branch refresh may discover and cache a missing one;
/// cleanup only reads local metadata and never contacts a remote.
pub async fn resolve(repo: &Path, lookup: DefaultBranchLookup) -> Result<String> {
    let remotes = git::run(repo, &["remote"]).await?;
    let remotes: Vec<_> = remotes.lines().collect();
    if remotes.is_empty() {
        let head = git::head_branch(repo, true, git::run)
            .await
            .context("local repository has no default branch; select a starting ref with --base")?;
        return head.context("repository HEAD does not name a local branch");
    }
    let remote = if remotes.contains(&"origin") {
        "origin"
    } else {
        ensure!(
            remotes.len() == 1,
            "repository default remote is ambiguous (no origin); select a starting ref with --base"
        );
        remotes[0]
    };
    let head = git::remote_ref(remote, "HEAD");
    let prefix = git::remote_ref(remote, "");
    if let Ok(target) = git::run(repo, &["symbolic-ref", "--quiet", &head]).await
        && let Some(branch) = target.trim_end_matches('\n').strip_prefix(&prefix)
    {
        return Ok(branch.to_owned());
    }
    ensure!(
        matches!(lookup, DefaultBranchLookup::Discover),
        "repository default branch is unknown; refresh {remote}/HEAD with git remote set-head"
    );
    let advertised = git::run_isolated(repo, &["ls-remote", "--symref", "--", remote, "HEAD"])
        .await
        .with_context(|| {
            format!(
                "discover {remote}'s default branch; use --base to select a starting point explicitly"
            )
        })?;
    let branch = advertised
        .lines()
        .find_map(|line| git::strip_local(line.strip_prefix("ref: ")?)?.strip_suffix("\tHEAD"))
        .context(
            "remote HEAD does not advertise a default branch; select a starting ref with --base",
        )?;
    git::run(
        repo,
        &["symbolic-ref", &head, &git::remote_ref(remote, branch)],
    )
    .await?;
    Ok(branch.to_owned())
}
