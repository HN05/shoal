//! Branch naming, upstream lookup and merges into the default branch for a registered repository.
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use shoal_refs::{BranchName, CommitId};

use crate::{
    forge::items::action::MergeMethod,
    fsutil::off_runtime,
    git::{
        self,
        merge_methods::{self, Applied},
        run_isolated as git_run,
    },
    model::{MergePlan, MergePush, MergedBranch, Repository},
};

/// The `(remote, refs/heads/...)` upstream of a local branch, if configured.
pub async fn upstream(repo: &Repository, local_ref: &str) -> Result<Option<(String, String)>> {
    let upstream = git::upstream(&repo.path, local_ref)
        .await?
        .with_context(|| format!("{local_ref} does not exist"))?;
    Ok(
        (!upstream.remote.is_empty() && git::strip_local(&upstream.merge).is_some())
            .then_some((upstream.remote, upstream.merge)),
    )
}

/// Suffix conflicting components with `-2`, `-3`, ... A branch at an ancestor
/// blocks all its descendants, so that component is suffixed rather than
/// repeatedly suffixing an unreachable leaf.
pub fn allocate_branch(name: &str, taken: &[BranchName]) -> BranchName {
    let mut prefix = String::new();
    let mut components = name.split('/').peekable();
    while let Some(component) = components.next() {
        let base = format!("{prefix}{component}");
        let mut candidate = base.clone();
        let mut suffix = 2_u64;
        let last = components.peek().is_none();
        while (last && git::branch::is_reserved_name(&candidate))
            || taken.iter().any(|existing| {
                *existing == candidate
                    || (last && existing.as_str().starts_with(&format!("{candidate}/")))
            })
        {
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        prefix = candidate;
        if !last {
            prefix.push('/');
        }
    }
    prefix.into()
}

/// A branch's checkout must be exactly on that branch and clean before Shoal
/// moves it.
pub async fn clean_branch(path: &Path, branch: &BranchName) -> Result<()> {
    // A detached HEAD has no symbolic ref; that is the same refusal.
    let head = git::head_branch(path).await.unwrap_or_default();
    ensure!(
        head.as_ref() == Some(branch),
        "checkout at {} is not on {branch}",
        path.display()
    );
    ensure!(
        git_run(
            path,
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--ignore-submodules=none"
            ]
        )
        .await?
        .is_empty(),
        "{branch} checkout has uncommitted or untracked changes; commit or clean them first"
    );
    Ok(())
}

/// The first Git operation a merge must not run over in `checkout`: an
/// unfinished merge, cherry-pick, revert, rebase or sequencer run, or a held
/// index lock.
pub async fn unfinished_operation(checkout: &Path) -> Result<Option<&'static str>> {
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-apply",
        "rebase-merge",
        "sequencer",
        "index.lock",
    ] {
        let path = git::git_path(checkout, marker).await?;
        if off_runtime::exists(&path).await? {
            return Ok(Some(marker));
        }
    }
    Ok(None)
}

/// Executed only by the merge worker, inside the tracked process group.
/// When `plan` pushes, the default branch then goes to its upstream.
pub async fn finish_merge(plan: MergePlan) -> Result<MergedBranch> {
    let default = plan.default_refresh.branch.clone();
    let target = plan.push.clone();
    let repo = plan.repo.path.clone();
    let mut merged = merge_branch(plan).await?;
    if let Some(MergePush { remote, reference }) = target {
        git_run(
            &repo,
            &[
                "push",
                "--",
                &remote,
                &format!("{}:{reference}", merged.commit),
            ],
        )
        .await
        .with_context(|| {
            format!(
                "{} merged into {default} ({}), but pushing {default} to {remote} failed",
                merged.branch, merged.commit
            )
        })?;
        merged.pushed_to = Some(remote);
    }
    Ok(merged)
}

async fn merge_branch(plan: MergePlan) -> Result<MergedBranch> {
    ensure_unmoved(&plan).await?;
    let (repo, previous, source) = (&plan.repo.path, &plan.default_refresh.commit, &plan.source);
    let contained = git::is_ancestor(
        repo,
        source.as_str(),
        previous.as_str(),
        git::isolated_command,
    )
    .await?;
    let (updated, fast_forward) = match plan.method {
        _ if contained => (false, true),
        MergeMethod::Merge => (true, merge_or_fast_forward(&plan).await?),
        MergeMethod::Squash => {
            let message = plan.message.as_deref();
            let squashed = merge_methods::squash(repo, previous, source, message).await?;
            (move_default(&plan, squashed).await?, false)
        }
        MergeMethod::Rebase => {
            let rebased = merge_methods::rebase(repo, previous, source).await?;
            (move_default(&plan, rebased).await?, false)
        }
    };
    let default_ref = git::local_ref(&plan.default_refresh.branch);
    let commit = git_run(repo, &["rev-parse", "--verify", &default_ref])
        .await?
        .trim()
        .into();
    Ok(merged_branch(plan, commit, updated, fast_forward))
}

/// Refuse a merge whose workspace branch or default branch moved since
/// `plan` was made.
async fn ensure_unmoved(plan: &MergePlan) -> Result<()> {
    let default_ref = git::local_ref(&plan.default_refresh.branch);
    clean_branch(&plan.workspace.path, &plan.branch).await?;
    ensure!(
        git_run(&plan.workspace.path, &["rev-parse", "HEAD"])
            .await?
            .trim()
            == plan.source,
        "workspace branch changed before merging; retry"
    );
    ensure!(
        git_run(&plan.repo.path, &["rev-parse", &default_ref])
            .await?
            .trim()
            == plan.default_refresh.commit,
        "default branch changed before merging; retry"
    );
    Ok(())
}

/// What merging `plan` left the default branch at `commit`.
fn merged_branch(
    plan: MergePlan,
    commit: CommitId,
    updated: bool,
    fast_forward: bool,
) -> MergedBranch {
    MergedBranch {
        workspace_id: plan.workspace.id,
        repository_id: plan.repo.id,
        branch: plan.branch,
        default_branch: plan.default_refresh.branch.clone(),
        previous_commit: plan.default_refresh.commit.clone(),
        commit,
        method: plan.method,
        updated,
        fast_forward,
        default_refresh: plan.default_refresh,
        pushed_to: None,
    }
}

/// Fast-forward the default branch to `plan`'s branch, or merge the branch
/// into it with a merge commit in its checkout; whether it fast-forwarded.
async fn merge_or_fast_forward(plan: &MergePlan) -> Result<bool> {
    let (branch, source) = (&plan.branch, &plan.source);
    let (repo, default) = (&plan.repo.path, &plan.default_refresh.branch);
    let previous = &plan.default_refresh.commit;
    let fast_forward = git::is_ancestor(
        repo,
        previous.as_str(),
        source.as_str(),
        git::isolated_command,
    )
    .await?;
    match &plan.checkout {
        Some(checkout) => {
            clean_branch(checkout, default).await?;
            let output = git::bounded(git::merge_commit(checkout, branch, source.as_str()))
                .capture()
                .await
                .context("merge into default branch")?;
            if !output.status.success() {
                // Leave the checkout as it was; conflicts belong in the workspace.
                let _ = git_run(checkout, &["merge", "--abort"]).await;
                bail!(
                    "merging {branch} into {default} failed; run git merge {default} in the workspace, resolve conflicts there, and retry\n{}{}",
                    String::from_utf8_lossy(&output.stdout).trim_end(),
                    String::from_utf8_lossy(&output.stderr).trim_end()
                );
            }
        }
        None => {
            ensure!(
                fast_forward,
                "{default} is not checked out and {branch} does not fast-forward it; run git merge {default} in the workspace, then retry"
            );
            git::fast_forward_local(repo, source.as_str(), &git::local_ref(default)).await?;
        }
    }
    Ok(fast_forward)
}

/// Fast-forward the default branch to the commit a squash or rebase of
/// `plan`'s branch made, if it made one; whether it did. One that does not
/// apply cleanly changes nothing and names what makes it apply.
async fn move_default(plan: &MergePlan, applied: Applied) -> Result<bool> {
    let (branch, default) = (&plan.branch, &plan.default_refresh.branch);
    let commit = match applied {
        Applied::Commit(commit) => commit,
        Applied::Unchanged => return Ok(false),
        Applied::Conflicts(report) => {
            let (merging, update) = match plan.method {
                MergeMethod::Rebase => (format!("rebasing {branch} onto {default}"), "rebase"),
                _ => (format!("squashing {branch} into {default}"), "merge"),
            };
            bail!(
                "{merging} does not apply cleanly, so nothing changed; run git {update} {default} in the workspace, resolve conflicts there, and retry\n{report}"
            );
        }
    };
    match &plan.checkout {
        Some(checkout) => {
            clean_branch(checkout, default).await?;
            git::run_without_submodules(
                checkout,
                &[
                    "merge",
                    "--ff-only",
                    "--no-squash",
                    "--no-stat",
                    "--no-autostash",
                    "--no-overwrite-ignore",
                    "--",
                    commit.as_str(),
                ],
            )
            .await
            .with_context(|| format!("move {default}'s checkout to {commit}"))?;
        }
        None => {
            let default_ref = git::local_ref(default);
            git::fast_forward_local(&plan.repo.path, commit.as_str(), &default_ref).await?;
        }
    }
    Ok(true)
}

/// The wrapper runs rollback only after stopping the merge process group. Keep
/// a completed merge, and refuse to reset a checkout whose branch/HEAD changed.
pub async fn rollback_merge(plan: &MergePlan) -> Result<()> {
    merge_methods::remove_rebase(&plan.repo.path).await?;
    let Some(checkout) = &plan.checkout else {
        return Ok(());
    };
    let branch = &plan.default_refresh.branch;
    ensure!(
        git::head_branch(checkout).await?.as_ref() == Some(branch),
        "default checkout changed branches during the merge; recover it manually"
    );
    let head = git_run(checkout, &["rev-parse", "HEAD"]).await?;
    if head.trim() != plan.default_refresh.commit {
        return Ok(());
    }
    // --merge restores the index and merge state without discarding unrelated
    // unstaged edits. Git refuses an index lock; never remove someone else's lock.
    git_run(
        checkout,
        &["reset", "--merge", plan.default_refresh.commit.as_str()],
    )
    .await
    .context("could not roll back interrupted merge; inspect the default checkout")?;
    // Git's external merge drivers use these temporary inputs in the checkout.
    // A signal can bypass Git's normal unlink; preserve any that predated us.
    for path in merge_temporaries(checkout)? {
        if !plan.merge_temporaries.contains(&path) {
            std::fs::remove_file(&path).context("remove interrupted Git merge input")?;
        }
    }
    Ok(())
}

pub fn merge_temporaries(checkout: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(checkout)? {
        let entry = entry?;
        let name = entry.file_name();
        if name
            .to_str()
            .and_then(|name| name.strip_prefix(".merge_file_"))
            .is_some_and(|suffix| {
                suffix.len() == 6 && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            && entry.file_type()?.is_file()
        {
            files.push(entry.path());
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests;
