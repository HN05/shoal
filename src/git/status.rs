//! Worktree state and refs a removal inspection compares to decide what to retain.
use std::{
    collections::HashMap,
    path::Path,
    sync::{LazyLock, Mutex, PoisonError},
};

use anyhow::{Context, Result, bail};

use super::run;
use crate::fsutil::off_runtime;

pub async fn porcelain(repo: &Path) -> Result<String> {
    run(repo, &["status", "--porcelain", "--untracked-files=normal"]).await
}

/// Whether the checkout holds work no remote keeps: changed or untracked
/// files, a stash, a Git operation under way, or commits on a local branch,
/// tag or detached HEAD that none of `kept`, the branches and tags the
/// remotes advertise, has.
pub async fn unretained_work(repo: &Path, kept: &[super::RemoteHead]) -> Result<bool> {
    if !porcelain(repo).await?.is_empty()
        || operation_in_progress(repo).await?
        || super::ref_exists(repo, "refs/stash", super::command).await?
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
pub async fn changed_files(repo: &Path) -> Result<String> {
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

/// Commits on HEAD that `base` does not contain.
pub async fn commits_since(repo: &Path, base: &str) -> Result<u64> {
    commits_between(repo, base, "HEAD").await
}

/// Commits reachable from `revision`.
pub async fn commits_of(repo: &Path, revision: &str) -> Result<u64> {
    Ok(run(repo, &["rev-list", "--count", revision])
        .await?
        .trim()
        .parse()?)
}

/// Commits on `revision` that `base` does not contain.
pub async fn commits_between(repo: &Path, base: &str, revision: &str) -> Result<u64> {
    let range = format!("{base}..{revision}");
    Ok(run(repo, &["rev-list", "--count", &range])
        .await?
        .trim()
        .parse()?)
}

/// Commits on `revision` that are reachable from neither a remote nor the given ref.
pub async fn unretained_commits(repo: &Path, revision: &str, retained: &[&str]) -> Result<u64> {
    let mut args = vec!["rev-list", "--count", revision, "--not", "--remotes"];
    args.extend(retained);
    Ok(run(repo, &args).await?.trim().parse()?)
}

/// The full ref of a local branch, when it exists.
pub async fn local_branch(repo: &Path, name: &str) -> Result<Option<String>> {
    let reference = super::local_ref(name);
    Ok(super::ref_exists(repo, &reference, super::command)
        .await?
        .then_some(reference))
}

/// The commit a local branch points at, or `None` when the branch is gone.
/// A broken ref or failed lookup stays an error.
pub async fn local_branch_commit(repo: &Path, name: &str) -> Result<Option<String>> {
    let reference = super::local_ref(name);
    if !super::ref_exists(repo, &reference, super::isolated_command).await? {
        return Ok(None);
    }
    super::resolve_commit(repo, &reference, super::run_isolated)
        .await
        .map(Some)
}

/// The tree HEAD records, to compare with other revisions by content.
pub async fn head_tree(repo: &Path) -> Result<String> {
    super::revision(repo, "HEAD^{tree}", run).await
}

/// Whether a merge, rebase, cherry-pick, revert or bisect is under way, or the
/// index has conflicts: state a snapshot of the files would not capture.
pub async fn operation_in_progress(repo: &Path) -> Result<bool> {
    const MARKERS: &[&str] = &[
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
        "rebase-merge",
        "rebase-apply",
    ];
    let git_dir = super::git_dir(repo).await?;
    for marker in MARKERS {
        if off_runtime::try_exists(&git_dir.join(marker)).await? {
            return Ok(true);
        }
    }
    Ok(!run(repo, &["ls-files", "--unmerged"]).await?.is_empty())
}

/// The default branch's refs that exist, local first: the local branch, its
/// upstream, and the default remote's remote-tracking branch, which a clone
/// keeps even without a local branch.
pub async fn default_refs(repo: &Path, name: &str) -> Result<Vec<String>> {
    let local = super::local_ref(name);
    let mut candidates = Vec::new();
    if let Some(upstream) = upstream_of(repo, &local).await? {
        candidates.extend([local, upstream]);
    }
    if let Some(remote) = super::default_branch::remote(repo).await? {
        candidates.push(super::remote_ref(&remote, name));
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
    let listed = run(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(upstream)",
            reference,
        ],
    )
    .await?;
    let upstream = listed.lines().find_map(|line| {
        let (name, upstream) = line.split_once('\0')?;
        (name == reference).then(|| upstream.to_owned())
    });
    // for-each-ref skips a broken ref with only a warning, where `show-ref`
    // fails, so `show-ref` confirms a branch the listing left out.
    if upstream.is_none() && super::ref_exists(repo, reference, super::command).await? {
        bail!("git for-each-ref did not list {reference}");
    }
    Ok(upstream)
}

/// Which of the full ref names in `references` exist, from one listing.
async fn existing_refs(repo: &Path, references: &[String]) -> Result<Vec<String>> {
    // Without patterns, for-each-ref would list every ref.
    if references.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["for-each-ref", "--format=%(refname)"];
    args.extend(references.iter().map(String::as_str));
    let listed = run(repo, &args).await?;
    let mut existing = Vec::new();
    for reference in references {
        // A pattern also matches the refs below it, so only exact names
        // count; `show-ref` confirms a ref the listing left out, as above.
        if listed.lines().any(|name| name == reference)
            || super::ref_exists(repo, reference, super::command).await?
        {
            existing.push(reference.clone());
        }
    }
    Ok(existing)
}

/// Whether any of `references` retains `revision`, whose tree is `tree`, by
/// recording that tree or already having its changes.
pub async fn retains(
    repo: &Path,
    references: &[String],
    revision: &str,
    tree: &str,
) -> Result<bool> {
    for reference in references {
        if records_tree(repo, reference, tree).await
            || merged_into(repo, revision, reference).await?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether any of the default branch `name`'s refs retains HEAD.
pub async fn default_branch_retains_head(repo: &Path, name: &str) -> Result<bool> {
    let references = default_refs(repo, name).await?;
    retains(repo, &references, "HEAD", &head_tree(repo).await?).await
}

/// Commits of `commit` that neither a remote nor the default branch `default`
/// keeps; none once the default branch has its changes.
pub async fn unkept_commits(repo: &Path, commit: &str, default: &str) -> Result<u64> {
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
    default: &str,
    present: &[super::RemoteHead],
) -> Result<u64> {
    let default_name = super::local_ref(default);
    let mut defaults: Vec<String> = local_branch(repo, default).await?.into_iter().collect();
    defaults.extend(
        present
            .iter()
            .filter(|head| head.name == default_name)
            .map(|head| head.commit.clone()),
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

/// Whether `target` already has `revision`'s changes: it is an ancestor, a
/// trial merge of it leaves the target's tree unchanged (squash merges), or every
/// commit since the fork point, none of them a merge, has an equivalent patch
/// on it (rebase merges).
///
/// The answer depends only on the two commits, so it is remembered by their
/// IDs: cleanup asks again on every pass, and `git cherry` reads every commit
/// since the fork point.
pub async fn merged_into(repo: &Path, revision: &str, target: &str) -> Result<bool> {
    static ANSWERS: LazyLock<Mutex<HashMap<(String, String), bool>>> =
        LazyLock::new(Mutex::default);
    let commits = run(
        repo,
        &[
            "rev-parse",
            &format!("{revision}^{{commit}}"),
            &format!("{target}^{{commit}}"),
        ],
    )
    .await?;
    let mut commits = commits.lines().map(str::to_owned);
    let key = (
        commits.next().context("revision has no commit")?,
        commits.next().context("target has no commit")?,
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
    let target_tree = super::revision(repo, &format!("{target}^{{tree}}"), run).await?;
    if trial_merge_tree(repo, head, target).await?.as_deref() == Some(target_tree.trim()) {
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

/// The tree a clean merge of `head` into `target` would record, or `None`
/// when it conflicts. `git merge-tree` writes objects only.
pub(crate) async fn trial_merge_tree(
    repo: &Path,
    head: &str,
    target: &str,
) -> Result<Option<String>> {
    let mut command = super::isolated_command_without_submodules(repo);
    command.args(["merge-tree", "--write-tree", "--no-messages", target, head]);
    let output = super::bounded(command).capture().await;
    if output.as_ref().ok().and_then(|output| output.status.code()) == Some(1) {
        return Ok(None);
    }
    let stdout = crate::subprocess::checked_output("git merge-tree", output)
        .context("trial merge; Git 2.38 or newer is required")?;
    Ok(Some(stdout.trim().to_owned()))
}

/// Whether `revision` records `tree`, given with or without Git's newline.
async fn records_tree(repo: &Path, revision: &str, tree: &str) -> bool {
    super::verify_revision(repo, &format!("{revision}^{{tree}}"), run)
        .await
        .is_ok_and(|other| other.trim() == tree.trim())
}

#[cfg(test)]
mod tests;
