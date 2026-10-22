//! Branch names, renaming and checkout fast-forwards.
use std::path::Path;

use anyhow::{Context, Result};

use crate::git;

/// Local and remote branch names that can conflict with a new local branch.
pub async fn names_in_use(repo: &Path) -> Result<Vec<String>> {
    let refs = git::run(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            git::LOCAL_REFS,
            git::REMOTE_REFS,
        ],
    )
    .await?;
    Ok(refs
        .lines()
        .filter_map(|reference| {
            git::strip_local(reference).or_else(|| {
                git::strip_remote(reference)?
                    .split_once('/')
                    .map(|(_, branch)| branch)
            })
        })
        .map(str::to_owned)
        .collect())
}

/// Create a local branch at `commit`; an existing branch is an error.
pub async fn create(repo: &Path, name: &str, commit: &str) -> Result<String> {
    git::run_isolated(repo, &["branch", "--no-track", "--", name, commit]).await
}

/// Delete a local branch only while it still points at `commit`, so a
/// branch that moved meanwhile stays; a symbolic ref goes itself, never its
/// target. The caller checks that no worktree has it checked out.
pub async fn delete_at(repo: &Path, name: &str, commit: &str) -> Result<String> {
    git::run_isolated(
        repo,
        &[
            "update-ref",
            "--no-deref",
            "-d",
            &git::local_ref(name),
            commit,
        ],
    )
    .await
}

/// Whether the local branch is a symbolic ref to another one.
pub async fn is_symbolic(repo: &Path, name: &str) -> Result<bool> {
    let reference = git::local_ref(name);
    let listing = git::run(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(symref)",
            &reference,
        ],
    )
    .await?;
    // The pattern also matches branches nested under the name.
    Ok(listing.lines().any(|line| {
        line.split_once('\0')
            .is_some_and(|(name, target)| name == reference && !target.is_empty())
    }))
}

/// Whether a rebase or bisect in some worktree started from the branch, or a
/// rebase will update it. Git shows that worktree's HEAD as detached, but the
/// operation returns to or rewrites the branch, so the branch is in use.
pub async fn in_operation(repo: &Path, name: &str) -> Result<bool> {
    let common = git::common_dir(repo).await?;
    let linked = match std::fs::read_dir(common.join("worktrees")) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let reference = git::local_ref(name);
    for dir in std::iter::once(common).chain(linked) {
        let read = |file: &str| {
            std::fs::read_to_string(dir.join(file)).map(|text| text.trim_end().to_owned())
        };
        // Rebases record full refs, `--update-refs` one per line among the
        // commits; bisects record the branch's short name.
        if read("rebase-merge/head-name").is_ok_and(|head| head == reference)
            || read("rebase-apply/head-name").is_ok_and(|head| head == reference)
            || read("rebase-merge/update-refs")
                .is_ok_and(|refs| refs.lines().any(|line| line == reference))
            || read("BISECT_START").is_ok_and(|start| start == name)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// When `commit` was made, in Unix seconds, and its subject line.
pub async fn commit_summary(repo: &Path, commit: &str) -> Result<(u64, String)> {
    let output = git::run(repo, &["log", "-1", "--format=%ct%x00%s", commit, "--"]).await?;
    let (time, subject) = output
        .trim_end_matches('\n')
        .split_once('\0')
        .context("git log printed no commit summary")?;
    Ok((time.parse()?, subject.to_owned()))
}

pub async fn rename(repo: &Path, from: &str, to: &str) -> Result<String> {
    git::run_isolated(repo, &["branch", "-m", "--", from, to]).await
}

/// Fast-forward a checkout without recursive submodule updates.
pub async fn fast_forward(repo: &Path, commit: &str) -> Result<String> {
    git::run_without_submodules(
        repo,
        &[
            "merge",
            "--ff-only",
            "--no-edit",
            "--no-stat",
            "--no-overwrite-ignore",
            commit,
        ],
    )
    .await
}
