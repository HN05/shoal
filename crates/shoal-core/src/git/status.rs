//! Worktree state and refs a removal inspection compares to decide what to retain.
use std::{
    collections::HashMap,
    path::Path,
    sync::{LazyLock, Mutex, PoisonError},
};

use anyhow::{Context, Result, bail};
use shoal_refs::{BranchName, CommitId};

use super::{default_branch::DefaultBranch, run};
use crate::fsutil::off_runtime;

pub async fn porcelain(repo: &Path) -> Result<String> {
    run(repo, &["status", "--porcelain", "--untracked-files=normal"]).await
}

const UNMERGED: [&str; 7] = ["DD", "AU", "UD", "UA", "DU", "AA", "UU"];

/// Whether the [`porcelain`] status `status` lists an unmerged path: the
/// index has conflicts.
pub(crate) fn lists_unmerged(status: &str) -> bool {
    (status.lines()).any(|line| line.get(..2).is_some_and(|code| UNMERGED.contains(&code)))
}

/// Whether the checkout holds work no remote keeps: changed or untracked
/// files, a stash, a Git operation under way, or commits on a local branch,
/// tag or detached HEAD that none of `kept`, the branches and tags the
/// remotes advertise, has.
pub async fn unretained_work(repo: &Path, kept: &[super::RemoteHead]) -> Result<bool> {
    let status = porcelain(repo).await?;
    if !status.is_empty()
        || operation_in_progress_with(repo, Some(lists_unmerged(&status))).await?
        || super::ref_exists(repo, "refs/stash").await?
    {
        return Ok(true);
    }
    let mut args = vec![
        "rev-list",
        "--count",
        "HEAD",
        "--branches",
        "--tags",
        "--not",
    ];
    args.extend(kept.iter().map(|head| head.commit.as_str()));
    let commits = run(repo, &args).await?;
    Ok(commits.trim() != "0")
}

/// Quote paths and list every untracked file for a user-facing preview.
pub(crate) async fn changed_files(repo: &Path) -> Result<String> {
    run(
        repo,
        &[
            "-c",
            "core.quotePath=true",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        ],
    )
    .await
}

pub async fn commits_since(repo: &Path, base: &str) -> Result<u64> {
    commits_between(repo, base, "HEAD").await
}

/// The subjects of the newest `limit` commits on HEAD that `base` does not
/// contain, newest first.
pub async fn subjects_since(repo: &Path, base: &str, limit: usize) -> Result<Vec<String>> {
    let (range, limit) = (format!("{base}..HEAD"), format!("--max-count={limit}"));
    let subjects = run(repo, &["log", "--format=%s", &limit, &range, "--"]).await?;
    Ok(subjects.lines().map(str::to_owned).collect())
}

pub async fn commits_of(repo: &Path, rev: &str) -> Result<u64> {
    Ok(run(repo, &["rev-list", "--count", rev])
        .await?
        .trim()
        .parse()?)
}

pub async fn commits_between(repo: &Path, base: &str, rev: &str) -> Result<u64> {
    let range = format!("{base}..{rev}");
    Ok(run(repo, &["rev-list", "--count", &range])
        .await?
        .trim()
        .parse()?)
}

pub async fn unretained_commits(repo: &Path, rev: &str, retained: &[&str]) -> Result<u64> {
    let mut args = vec!["rev-list", "--count", rev, "--not", "--remotes"];
    args.extend(retained);
    Ok(run(repo, &args).await?.trim().parse()?)
}

pub(crate) async fn local_branch(repo: &Path, name: &BranchName) -> Result<Option<String>> {
    let reference = super::local_ref(name);
    Ok(super::ref_exists(repo, &reference)
        .await?
        .then_some(reference))
}

/// The commit a local branch points at, or `None` when the branch is gone.
/// A broken ref or failed lookup stays an error.
pub async fn local_branch_commit(repo: &Path, name: &BranchName) -> Result<Option<CommitId>> {
    let reference = super::local_ref(name);
    if !super::ref_exists(repo, &reference).await? {
        return Ok(None);
    }
    super::resolve_commit(repo, &reference, super::run_isolated)
        .await
        .map(Some)
}

pub(crate) async fn head_tree(repo: &Path) -> Result<String> {
    super::revision(repo, "HEAD^{tree}", run).await
}

/// Whether a merge, rebase, cherry-pick, revert or bisect is under way, or the
/// index has conflicts: state a snapshot of the files would not capture.
pub async fn operation_in_progress(repo: &Path) -> Result<bool> {
    operation_in_progress_with(repo, None).await
}

/// [`operation_in_progress`], where `unmerged` says whether the index has
/// conflicts, as a status the caller read listed them, or `None` to read
/// them.
pub(crate) async fn operation_in_progress_with(
    repo: &Path,
    unmerged: Option<bool>,
) -> Result<bool> {
    let git_dir = super::git_dir(repo).await?;
    Ok(operation_marked(&git_dir).await?
        || match unmerged {
            Some(unmerged) => unmerged,
            None => has_unmerged(repo).await?,
        })
}

/// [`operation_in_progress`] in the checkout at `repo`, whose own Git
/// directory the caller read as `git_dir`.
pub async fn operation_in_progress_at(repo: &Path, git_dir: &Path) -> Result<bool> {
    Ok(operation_marked(git_dir).await? || has_unmerged(repo).await?)
}

async fn operation_marked(git_dir: &Path) -> Result<bool> {
    pub(crate) const MARKERS: &[&str] = &[
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
        "rebase-merge",
        "rebase-apply",
    ];
    for marker in MARKERS {
        if off_runtime::try_exists(&git_dir.join(marker)).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn has_unmerged(repo: &Path) -> Result<bool> {
    Ok(!run(repo, &["ls-files", "--unmerged"]).await?.is_empty())
}

/// The default branch's refs that exist, local first: the local branch, its
/// upstream, and the default remote's remote-tracking branch, which a clone
/// keeps even without a local branch.
pub(crate) async fn default_refs(repo: &Path, default: &DefaultBranch) -> Result<Vec<String>> {
    let local = super::local_ref(&default.name);
    let mut candidates = Vec::new();
    if let Some(upstream) = upstream_of(repo, &local).await? {
        candidates.extend([local, upstream]);
    }
    if let Some(remote) = &default.remote {
        candidates.push(super::remote_ref(remote, default.name.as_str()));
    }
    let mut refs: Vec<String> = Vec::new();
    for reference in candidates {
        if (reference.starts_with(super::LOCAL_REFS) || reference.starts_with(super::REMOTE_REFS))
            && !refs.contains(&reference)
        {
            refs.push(reference);
        }
    }
    let existing = existing_refs(repo, &refs).await?;
    refs.retain(|reference| existing.contains(reference));
    Ok(refs)
}

/// The upstream the existing local branch `reference` tracks, empty when it
/// tracks none, or `None` when the branch does not exist.
async fn upstream_of(repo: &Path, reference: &str) -> Result<Option<String>> {
    let upstream = super::upstream(repo, reference).await?;
    let upstream = upstream.map(|upstream| upstream.tracking);
    // A listing skips a broken ref with only a warning, where an existence
    // check fails, so the check confirms a branch the listing left out.
    if upstream.is_none() && super::ref_exists(repo, reference).await? {
        bail!("the upstream of {reference} could not be read");
    }
    Ok(upstream)
}

/// Which of the full ref names in `references` exist, from one listing.
async fn existing_refs(repo: &Path, references: &[String]) -> Result<Vec<String>> {
    let patterns: Vec<&str> = references.iter().map(String::as_str).collect();
    let listed = super::refs::list(repo, &patterns).await?;
    let mut existing = Vec::new();
    for reference in references {
        // A pattern also matches the refs below it, so only exact names
        // count; `show-ref` confirms a ref the listing left out, as above.
        if listed.iter().any(|listed| listed.name == *reference)
            || super::ref_exists(repo, reference).await?
        {
            existing.push(reference.clone());
        }
    }
    Ok(existing)
}

/// Whether any of `references` retains `rev`, whose tree is `tree`, by
/// recording that tree or already having its changes.
pub(crate) async fn retains(
    repo: &Path,
    references: &[String],
    rev: &str,
    tree: &str,
) -> Result<bool> {
    for reference in references {
        if records_tree(repo, reference, tree).await || merged_into(repo, rev, reference).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn default_branch_retains_head(repo: &Path, default: &DefaultBranch) -> Result<bool> {
    let references = default_refs(repo, default).await?;
    retains(repo, &references, "HEAD", &head_tree(repo).await?).await
}

/// Commits of `commit` that neither a remote nor the default branch `default`
/// keeps; none once the default branch has its changes.
pub async fn unkept_commits(repo: &Path, commit: &str, default: &DefaultBranch) -> Result<u64> {
    let references = default_refs(repo, default).await?;
    let tree = super::revision(repo, &format!("{commit}^{{tree}}"), run).await?;
    if retains(repo, &references, commit, &tree).await? {
        return Ok(0);
    }
    let retained: Vec<_> = references.first().map(String::as_str).into_iter().collect();
    unretained_commits(repo, commit, &retained).await
}

/// Commits of `commit` that neither a branch in `present` nor the default
/// branch `default` keeps; none once the default branch has its changes.
/// `present` holds advertised heads whose commits are here, so no stale or
/// unmapped remote-tracking ref can vouch for a commit a remote no longer has.
pub async fn unkept_commits_by(
    repo: &Path,
    commit: &str,
    default: &BranchName,
    present: &[super::RemoteHead],
) -> Result<u64> {
    let default_name = super::local_ref(default);
    let mut defaults: Vec<String> = local_branch(repo, default).await?.into_iter().collect();
    defaults.extend(
        present
            .iter()
            .filter(|head| head.name == default_name)
            .map(|head| head.commit.to_string()),
    );
    let tree = super::revision(repo, &format!("{commit}^{{tree}}"), run).await?;
    if retains(repo, &defaults, commit, &tree).await? {
        return Ok(0);
    }
    let mut args = vec!["rev-list", "--count", commit, "--not"];
    args.extend(defaults.iter().map(String::as_str));
    args.extend(present.iter().map(|head| head.commit.as_str()));
    Ok(run(repo, &args).await?.trim().parse()?)
}

/// Whether `target` already has `rev`'s changes: it is an ancestor, or
/// shares history with the target and a trial merge of it leaves the
/// target's tree unchanged (squash merges), or every
/// commit since the fork point, none of them a merge, has an equivalent patch
/// on it (rebase merges).
///
/// The answer depends only on the two commits, so it is remembered by their
/// IDs: cleanup asks again on every pass, and `git cherry` reads every commit
/// since the fork point.
pub async fn merged_into(repo: &Path, rev: &str, target: &str) -> Result<bool> {
    pub(crate) static ANSWERS: LazyLock<Mutex<HashMap<(String, String), bool>>> =
        LazyLock::new(Mutex::default);
    let key = (
        super::resolve_commit(repo, rev, run).await?.to_string(),
        super::resolve_commit(repo, target, run).await?.to_string(),
    );
    let answers = || ANSWERS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(&merged) = answers().get(&key) {
        return Ok(merged);
    }
    let merged = target_has_changes(repo, &key.0, &key.1).await?;
    let mut answers = answers();
    if answers.len() >= 4096 {
        answers.clear();
    }
    answers.insert(key, merged);
    Ok(merged)
}

async fn target_has_changes(repo: &Path, head: &str, target: &str) -> Result<bool> {
    if super::is_ancestor(repo, head, target, super::command).await? {
        return Ok(true);
    }
    // Without a merge base there is nothing to merge: an unrelated history
    // brings changes of its own.
    if !super::predicates::share_history(repo, head, target).await? {
        return Ok(false);
    }
    let target_tree = super::revision(repo, &format!("{target}^{{tree}}"), run).await?;
    let merged = super::merge_tree::merged_tree(repo, target, head)
        .await
        .context("trial merge")?;
    if merged.as_deref() == Some(target_tree.trim()) {
        return Ok(true);
    }
    // `git cherry` skips merge commits, whose conflict resolutions can carry
    // changes of their own; only a history without them can prove equivalence.
    let range = format!("{target}..{head}");
    let merges = run(repo, &["rev-list", "--count", "--merges", &range]).await?;
    if merges.trim() != "0" {
        return Ok(false);
    }
    let cherry = run(repo, &["cherry", target, head]).await?;
    Ok(cherry.lines().all(|line| line.starts_with('-')))
}

/// Whether `rev` records `tree`, given with or without Git's newline.
async fn records_tree(repo: &Path, rev: &str, tree: &str) -> bool {
    super::verify_revision(repo, &format!("{rev}^{{tree}}"), run)
        .await
        .is_ok_and(|other| other.trim() == tree.trim())
}

#[cfg(test)]
mod tests;
