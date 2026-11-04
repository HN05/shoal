//! Undoing a local merge into the default branch: the history queries the
//! daemon checks an undo against, and the moves the unmerge worker makes.
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use crate::{
    fsutil::off_runtime,
    git::{self, repo::clean_branch, run_isolated as git_run},
    model::{UnmergePlan, Unmerged},
};

/// The commits of `tip`'s first-parent history down to `base`, newest
/// first and without `base`; `None` when that history does not pass
/// through `base`.
pub async fn first_parent_chain(repo: &Path, tip: &str, base: &str) -> Result<Option<Vec<String>>> {
    let listed = git_run(
        repo,
        &["rev-list", "--first-parent", tip, &format!("^{base}"), "--"],
    )
    .await?;
    let chain: Vec<String> = listed.lines().map(str::to_owned).collect();
    let Some(oldest) = chain.last() else {
        return Ok(
            (tip_commit(repo, tip).await? == tip_commit(repo, base).await?).then_some(chain),
        );
    };
    let parent = git_run(
        repo,
        &["rev-parse", "--verify", &format!("{oldest}^1^{{commit}}")],
    )
    .await
    .unwrap_or_default();
    Ok((parent.trim() == tip_commit(repo, base).await?).then_some(chain))
}

/// The parents of `commit` other than its first, which a merge commit
/// brought in.
pub async fn merged_parents(repo: &Path, commit: &str) -> Result<Vec<String>> {
    let listed = git_run(
        repo,
        &["rev-list", "--parents", "--max-count=1", commit, "--"],
    )
    .await?;
    Ok(listed
        .split_whitespace()
        .skip(2)
        .map(str::to_owned)
        .collect())
}

/// Whether the merge commit `commit` records changes its parents do not
/// merge into by themselves, such as a conflict resolution or an edit made
/// while merging, which replaying it would drop. A merge of more than two
/// parents counts as one.
pub async fn merge_has_own_changes(repo: &Path, commit: &str) -> Result<bool> {
    let parents = git_run(
        repo,
        &["rev-list", "--parents", "--max-count=1", commit, "--"],
    )
    .await?;
    let [_, first, second] = parents.split_whitespace().collect::<Vec<_>>()[..] else {
        return Ok(true);
    };
    let Some(merged) = git::status::trial_merge_tree(repo, second, first).await? else {
        return Ok(true);
    };
    let recorded = git_run(repo, &["rev-parse", &format!("{commit}^{{tree}}")]).await?;
    Ok(recorded.trim() != merged)
}

/// Whether `candidate` holds any commit of the branch merged at `source`
/// onto `previous`: a common ancestor of `candidate` and `source` that
/// `previous` lacks belongs to that branch.
pub async fn holds_commits_of(
    repo: &Path,
    candidate: &str,
    source: &str,
    previous: &str,
) -> Result<bool> {
    let mut command = git::isolated_command(repo);
    command.args(["merge-base", "--all", candidate, source]);
    let output = git::bounded(command).capture().await?;
    // Exit 1 with no output: unrelated histories share nothing.
    if output.status.code() == Some(1) && output.stdout.is_empty() {
        return Ok(false);
    }
    ensure!(
        output.status.success(),
        "git merge-base failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    for base in String::from_utf8_lossy(&output.stdout).lines() {
        if !git::is_ancestor(repo, base, previous, git::isolated_command).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Executed only by the unmerge worker, inside the tracked process group:
/// move the default branch back to before the merge, or replay the history
/// after it onto that commit, then recreate the merged branch if it is gone.
pub async fn finish_unmerge(plan: UnmergePlan) -> Result<Unmerged> {
    let default = &plan.default_branch;
    let current = tip_commit(&plan.repo.path, &git::local_ref(default)).await?;
    ensure!(
        current == plan.tip,
        "{default} changed before the unmerge; retry"
    );
    let replayed = match plan.tip == plan.result {
        true => {
            move_back(&plan).await?;
            0
        }
        false => replay(&plan).await?,
    };
    let commit = tip_commit(&plan.repo.path, &git::local_ref(default)).await?;
    let branch_recreated = recreate_branch(&plan).await?;
    Ok(Unmerged {
        workspace_id: plan.workspace.id,
        repository_id: plan.repo.id,
        branch: plan.branch,
        default_branch: plan.default_branch,
        source: plan.source,
        previous_commit: plan.tip,
        commit,
        replayed,
        branch_recreated,
    })
}

/// Move the default branch from the merge's result back to the commit
/// before it, in its checkout when it has one.
async fn move_back(plan: &UnmergePlan) -> Result<()> {
    let default = &plan.default_branch;
    match &plan.checkout {
        Some(checkout) => {
            clean_branch(checkout, default).await?;
            git_run(checkout, &["reset", "--keep", &plan.previous])
                .await
                .with_context(|| format!("move {default} back to {}", plan.previous))?;
        }
        None => {
            git_run(
                &plan.repo.path,
                &[
                    "update-ref",
                    "-m",
                    "shoal unmerge",
                    &git::local_ref(default),
                    &plan.previous,
                    &plan.result,
                ],
            )
            .await
            .with_context(|| format!("move {default} back to {}", plan.previous))?;
        }
    }
    Ok(())
}

/// Replay the default branch's history after the merge onto the commit
/// before it, keeping each later merge commit, and return how many commits
/// it replayed. A replay that conflicts, or that would bring the merged
/// commits back, leaves the default branch as it was.
async fn replay(plan: &UnmergePlan) -> Result<u64> {
    let default = &plan.default_branch;
    let checkout = plan.checkout.as_deref().with_context(|| {
        format!("{default} is not checked out; undoing an older merge replays later commits in its checkout")
    })?;
    clean_branch(checkout, default).await?;
    let output = git::bounded(rebase_command(checkout, &plan.previous, &plan.result))
        .capture()
        .await
        .context("replay the default branch")?;
    if !output.status.success() {
        let _ = git_run(checkout, &["rebase", "--abort"]).await;
        bail!(
            "replaying the commits after {} merged onto {default} failed, so nothing changed; unmerge the later merges first\n{}{}",
            plan.branch,
            String::from_utf8_lossy(&output.stdout).trim_end(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }
    let commit = tip_commit(checkout, "HEAD").await?;
    if holds_commits_of(checkout, &commit, &plan.source, &plan.previous).await? {
        git_run(checkout, &["reset", "--keep", &plan.tip])
            .await
            .context("restore the default branch after a replay that kept the merged commits")?;
        bail!(
            "replaying the commits after {} merged would bring its commits back into {default}, so nothing changed; unmerge the merges that contain them first",
            plan.branch
        );
    }
    let replayed = first_parent_chain(checkout, &commit, &plan.previous)
        .await?
        .map_or(0, |chain| chain.len());
    Ok(replayed as u64)
}

/// `git rebase` replaying `checkout`'s branch after `upstream` onto `onto`,
/// keeping merge commits and commits that become empty, without hooks.
fn rebase_command(checkout: &Path, onto: &str, upstream: &str) -> tokio::process::Command {
    let mut command = git::isolated_command_without_submodules(checkout);
    command.args([
        "rebase",
        "--rebase-merges",
        "--empty=keep",
        "--no-autosquash",
        "--no-autostash",
        "--no-update-refs",
        "--onto",
        onto,
        upstream,
    ]);
    // Replaying merge commits reuses their messages; nothing may wait on an editor.
    command.env("GIT_EDITOR", "true");
    command
}

/// Recreate the merged branch at its merged commit when cleanup deleted it,
/// so its commits stay on a branch; whether it did.
async fn recreate_branch(plan: &UnmergePlan) -> Result<bool> {
    let reference = git::local_ref(&plan.branch);
    if git::ref_exists(&plan.repo.path, &reference, git::isolated_command).await? {
        return Ok(false);
    }
    git::branch::create(&plan.repo.path, &plan.branch, &plan.source).await?;
    Ok(true)
}

/// The wrapper runs rollback only after stopping the unmerge process group:
/// a replay it interrupted is aborted, which puts the default branch back.
pub async fn rollback_unmerge(plan: &UnmergePlan) -> Result<()> {
    let Some(checkout) = &plan.checkout else {
        return Ok(());
    };
    abort_replay(checkout)
        .await
        .context("could not roll back the interrupted unmerge; inspect the default checkout")
}

/// Abort the replay `checkout` is in, if any. Its rebase directory alone
/// says so: a replay stopped while merging also leaves `MERGE_HEAD`.
async fn abort_replay(checkout: &Path) -> Result<()> {
    let rebase = git::metadata_path(checkout, "rebase-merge", git_run).await?;
    if off_runtime::exists(&checkout.join(rebase.trim())).await? {
        git_run(checkout, &["rebase", "--abort"]).await?;
    }
    Ok(())
}

/// The commit `revision` names.
pub async fn tip_commit(repo: &Path, revision: &str) -> Result<String> {
    Ok(git_run(
        repo,
        &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
    )
    .await?
    .trim()
    .to_owned())
}

#[cfg(test)]
mod tests;
