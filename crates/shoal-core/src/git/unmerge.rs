//! Undoing a local merge into the default branch: the history queries the
//! daemon checks an undo against, and the moves the unmerge worker makes.
use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result, bail, ensure};
use shoal_refs::{BranchName, CommitId};

use crate::{
    forge::items::action::MergeMethod,
    git::{self, merge_tree, repo::clean_branch, run_isolated as git_run, scratch},
    model::{UnmergePlan, Unmerged},
};

/// The commits of `tip`'s first-parent history down to `base`, newest
/// first and without `base`; `None` when that history does not pass
/// through `base`.
pub async fn first_parent_chain(
    repo: &Path,
    tip: &str,
    base: &str,
) -> Result<Option<Vec<CommitId>>> {
    let listed = git_run(
        repo,
        &["rev-list", "--first-parent", tip, &format!("^{base}"), "--"],
    )
    .await?;
    let chain: Vec<CommitId> = listed.lines().map(CommitId::from).collect();
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
    Ok((tip_commit(repo, base).await? == parent.trim()).then_some(chain))
}

/// The parents of `rev`'s commit other than its first, which a merge
/// commit brought in.
pub async fn merged_parents(repo: &Path, rev: &str) -> Result<Vec<CommitId>> {
    let listed = git_run(repo, &["rev-list", "--parents", "--max-count=1", rev, "--"]).await?;
    Ok(listed
        .split_whitespace()
        .skip(2)
        .map(CommitId::from)
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
    let Some(merged) = git::merge_tree::merged_tree(repo, first, second).await? else {
        return Ok(true);
    };
    let recorded = git_run(repo, &["rev-parse", &format!("{commit}^{{tree}}")]).await?;
    Ok(recorded.trim() != merged)
}

/// The commit a merge of `source` onto `previous` by `method` left the
/// default branch at, found in `tip`'s first-parent history. `None` when
/// that history shows no such merge, as after one that merged nothing.
pub async fn merge_result(
    repo: &Path,
    tip: &str,
    previous: &CommitId,
    source: &CommitId,
    method: MergeMethod,
) -> Result<Option<CommitId>> {
    match method {
        MergeMethod::Merge => merged(repo, tip, previous, source).await,
        MergeMethod::Squash => squashed(repo, tip, previous, source).await,
        MergeMethod::Rebase => rebased(repo, tip, previous, source).await,
    }
}

/// The commit a merge of `source` onto `previous` left in `tip`'s
/// first-parent history: `source` itself after a fast-forward, else the
/// merge commit of the two.
async fn merged(
    repo: &Path,
    tip: &str,
    previous: &CommitId,
    source: &CommitId,
) -> Result<Option<CommitId>> {
    // A fast-forward needs `source` on the tip's first-parent history and
    // `previous` among its ancestors, though not on its first parents: a
    // branch that merged the default branch in holds it as a second parent.
    let fast_forward = source != previous
        && git::is_ancestor(
            repo,
            previous.as_str(),
            source.as_str(),
            git::isolated_command,
        )
        .await?
        && first_parent_chain(repo, tip, source.as_str())
            .await?
            .is_some();
    if fast_forward {
        return Ok(Some(source.clone()));
    }
    let Some(first) = first_after(repo, tip, previous).await? else {
        return Ok(None);
    };
    let merged = merged_parents(repo, first.as_str()).await?;
    Ok(merged
        .iter()
        .any(|parent| parent == source)
        .then_some(first))
}

/// The commit a squash merge of `source` onto `previous` left in `tip`'s
/// first-parent history: the one commit on `previous` whose tree is the
/// merge of the two.
async fn squashed(
    repo: &Path,
    tip: &str,
    previous: &CommitId,
    source: &CommitId,
) -> Result<Option<CommitId>> {
    let Some(first) = first_after(repo, tip, previous).await? else {
        return Ok(None);
    };
    if !merged_parents(repo, first.as_str()).await?.is_empty() {
        return Ok(None);
    }
    let merged = merge_tree::merged_tree(repo, previous.as_str(), source.as_str()).await?;
    let recorded = merge_tree::tree_of(repo, first.as_str()).await?;
    Ok((merged == Some(recorded)).then_some(first))
}

/// The last commit a rebase merge of `source` onto `previous` left in
/// `tip`'s first-parent history: in the chain on `previous` whose commits
/// each copy one of the branch's own, as [`authored_identity`] tells them,
/// since a copy's patch differs where the rebase moved a change to a
/// renamed file, the last whose files are the merge of the two, as a clean
/// rebase leaves them. A commit of the branch with no copy there is one the
/// rebase dropped once it became empty, as when the default branch had its
/// change in a squash of several; a later commit that copies its authorship
/// changes the files, so it is no part of the merge.
async fn rebased(
    repo: &Path,
    tip: &str,
    previous: &CommitId,
    source: &CommitId,
) -> Result<Option<CommitId>> {
    let Some(chain) = first_parent_chain(repo, tip, previous.as_str()).await? else {
        return Ok(None);
    };
    let mut originals: HashMap<String, usize> = HashMap::new();
    for original in rebased_commits(repo, previous, source).await? {
        *originals
            .entry(authored_identity(repo, original.as_str()).await?)
            .or_default() += 1;
    }
    let Some(merged) = merge_tree::merged_tree(repo, previous.as_str(), source.as_str()).await?
    else {
        return Ok(None);
    };
    let mut last = None;
    for copy in chain.into_iter().rev() {
        match originals.get_mut(&authored_identity(repo, copy.as_str()).await?) {
            Some(left @ 1..) => *left -= 1,
            _ => break,
        }
        if merge_tree::tree_of(repo, copy.as_str()).await? == merged {
            last = Some(copy);
        }
    }
    Ok(last)
}

/// The commits of `source` a rebase onto `previous` copies, in the order
/// it copies them: those `previous` lacks, but merges and those whose
/// change `previous` already has.
async fn rebased_commits(
    repo: &Path,
    previous: &CommitId,
    source: &CommitId,
) -> Result<Vec<CommitId>> {
    let range = format!("{previous}...{source}");
    let listed = git_run(
        repo,
        &[
            "rev-list",
            "--reverse",
            "--topo-order",
            "--no-merges",
            "--cherry-pick",
            "--right-only",
            &range,
            "--",
        ],
    )
    .await?;
    Ok(listed.lines().map(CommitId::from).collect())
}

/// The first commit after `previous` in `tip`'s first-parent history.
async fn first_after(repo: &Path, tip: &str, previous: &CommitId) -> Result<Option<CommitId>> {
    let chain = first_parent_chain(repo, tip, previous.as_str()).await?;
    Ok(chain.and_then(|mut chain| chain.pop()))
}

/// Whether the upstream of `default`, as the last fetch saw it, holds
/// `commit`, so others may have it.
pub async fn pushed(repo: &Path, default: &BranchName, commit: &str) -> Result<bool> {
    let Some(tracking) = git::existing_branch::tracking_ref(repo, default).await? else {
        return Ok(false);
    };
    git::is_ancestor(repo, commit, &tracking, git::isolated_command).await
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
    ensure_unchanged(&plan).await?;
    let replayed = match plan.tip == plan.result {
        true => {
            move_default(&plan, &plan.previous).await?;
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

/// Refuse once the default branch is no longer at the tip the unmerge
/// planned for, as when a commit was made in its checkout meanwhile, which
/// moving it would drop, or once its upstream has the merge, as after a
/// push or fetch meanwhile, since only a revert undoes that.
async fn ensure_unchanged(plan: &UnmergePlan) -> Result<()> {
    let (repo, default) = (&plan.repo.path, &plan.default_branch);
    ensure!(
        !pushed(repo, default, plan.result.as_str()).await?,
        "{default}'s upstream got the merge of {} during the unmerge, so nothing changed; retry to see how to revert it",
        plan.branch
    );
    let current = tip_commit(repo, &git::local_ref(default)).await?;
    ensure!(
        current == plan.tip,
        "{default} changed during the unmerge, so nothing changed; retry"
    );
    Ok(())
}

/// Whether `checkout` is on `branch` at `commit`, read last before moving
/// it: a commit made or a branch switched there meanwhile would otherwise
/// be dropped or rewound.
pub(crate) async fn checkout_at(
    checkout: &Path,
    branch: &BranchName,
    commit: &CommitId,
) -> Result<bool> {
    let head = git::head_branch(checkout).await.unwrap_or_default();
    Ok(head.as_ref() == Some(branch) && tip_commit(checkout, "HEAD").await? == *commit)
}

/// What a replay keeps of `commit`, which a rebase carries over to the
/// commit it makes in its place: its author, author date and message, the
/// commits it merged, and the lines it changes from its first parent, by
/// the patch ID of a diff without context, which taking out an earlier
/// merge's nearby changes leaves as it was.
pub async fn replay_identity(repo: &Path, commit: &str) -> Result<String> {
    let metadata = authored_identity(repo, commit).await?;
    let merged = (merged_parents(repo, commit).await?.iter())
        .map(CommitId::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let mut show = git::isolated_command(repo);
    show.args([
        "show",
        "--no-color",
        "--no-ext-diff",
        "--patch",
        "--unified=0",
        "--diff-merges=first-parent",
        "--format=commit %H",
        commit,
        "--",
    ]);
    let patch = git::bounded(show).output().await?;
    let mut patch_id = git::isolated_command(repo);
    patch_id.args(["patch-id", "--stable"]);
    let id = git::bounded(patch_id).input(patch).output().await?;
    let id = id.split_whitespace().next().unwrap_or_default();
    Ok(format!("{metadata}\0{merged}\0{id}"))
}

/// What a rebase keeps of `commit` in the copy it makes: its author, author
/// date and message.
async fn authored_identity(repo: &Path, commit: &str) -> Result<String> {
    let format = "--format=%an%x00%ae%x00%ad%x00%B";
    git_run(repo, &["show", "-s", format, commit, "--"]).await
}

/// Whether `base..tip` holds a merge commit, which `git revert` takes only
/// with a mainline.
pub async fn merges_between(repo: &Path, base: &str, tip: &str) -> Result<bool> {
    let range = format!("{base}..{tip}");
    let count = git_run(repo, &["rev-list", "--count", "--merges", &range, "--"]).await?;
    Ok(count.trim() != "0")
}

/// Move the default branch from the tip the unmerge planned for to `target`:
/// in its checkout when it has one, which first refuses ignored files there
/// that `target`'s files would overwrite, since Git overwrites them unasked.
async fn move_default(plan: &UnmergePlan, target: &CommitId) -> Result<()> {
    let default = &plan.default_branch;
    let Some(checkout) = &plan.checkout else {
        ensure_unchanged(plan).await?;
        // Moving the ref under a checkout would leave its files behind.
        ensure!(
            git::checkout_of(&plan.repo.path, default, git_run)
                .await?
                .is_none(),
            "{default} was checked out during the unmerge, so nothing changed; retry"
        );
        let reference = git::local_ref(default);
        let args = [
            "update-ref",
            "-m",
            "shoal unmerge",
            &reference,
            target.as_str(),
            plan.tip.as_str(),
        ];
        git_run(&plan.repo.path, &args)
            .await
            .with_context(|| format!("move {default} to {target}"))?;
        return Ok(());
    };
    clean_branch(checkout, default).await?;
    let collisions =
        git::checkpoint::ignored_collisions(checkout, target.as_str(), target.as_str()).await?;
    ensure!(
        collisions.is_empty(),
        "undoing the merge would overwrite ignored files in {default}'s checkout: {}; move them away first, nothing changed",
        collisions.join(", ")
    );
    ensure_unchanged(plan).await?;
    ensure!(
        checkout_at(checkout, default, &plan.tip).await?,
        "{default}'s checkout changed during the unmerge, so nothing changed; retry"
    );
    git_run(checkout, &["reset", "--keep", target.as_str()])
        .await
        .with_context(|| format!("move {default} to {target}"))?;
    Ok(())
}

/// Replay the default branch's history after the merge onto the commit
/// before it, keeping each later merge commit, then move the default branch
/// to the result, and return how many commits it replayed. The replay runs
/// in a worktree of its own, so one that conflicts, or that would bring the
/// merged commits back, leaves the default branch and its checkout as they
/// were.
async fn replay(plan: &UnmergePlan) -> Result<u64> {
    let repo = &plan.repo.path;
    let worktree = scratch::UNMERGE.add(repo, &plan.tip).await?;
    let replayed = replay_in(plan, &worktree).await;
    scratch::UNMERGE.remove(repo).await?;
    let (commit, replayed) = replayed?;
    move_default(plan, &commit).await?;
    Ok(replayed)
}

/// Replay the history after the merge in `worktree`, detached at the tip:
/// the commit it makes and how many first-parent commits it replayed.
async fn replay_in(plan: &UnmergePlan, worktree: &Path) -> Result<(CommitId, u64)> {
    let default = &plan.default_branch;
    let output = git::bounded(rebase_command(
        worktree,
        plan.previous.as_str(),
        plan.result.as_str(),
    ))
    .capture()
    .await
    .context("replay the default branch")?;
    if !output.status.success() {
        bail!(
            "replaying the commits after {} merged onto {default} failed, so nothing changed; unmerge the later merges first\n{}{}",
            plan.branch,
            String::from_utf8_lossy(&output.stdout).trim_end(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }
    let commit = tip_commit(worktree, "HEAD").await?;
    if holds_commits_of(
        worktree,
        commit.as_str(),
        plan.source.as_str(),
        plan.previous.as_str(),
    )
    .await?
    {
        bail!(
            "replaying the commits after {} merged would bring its commits back into {default}, so nothing changed; unmerge the merges that contain them first",
            plan.branch
        );
    }
    let replayed = first_parent_chain(worktree, commit.as_str(), plan.previous.as_str())
        .await?
        .map_or(0, |chain| chain.len());
    Ok((commit, replayed as u64))
}

/// `git rebase` replaying `worktree`'s HEAD after `upstream` onto `onto`,
/// keeping merge commits and commits that become empty, without hooks.
fn rebase_command(worktree: &Path, onto: &str, upstream: &str) -> tokio::process::Command {
    let mut command = git::isolated_command_without_submodules(worktree);
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
    if git::ref_exists(&plan.repo.path, &reference).await? {
        return Ok(false);
    }
    git::branch::create(&plan.repo.path, &plan.branch, plan.source.as_str()).await?;
    Ok(true)
}

/// The wrapper runs rollback only after stopping the unmerge process group:
/// a replay it interrupted ran in a worktree of its own, which goes, and
/// the default branch moves only once the replay is done.
pub async fn rollback_unmerge(plan: &UnmergePlan) -> Result<()> {
    scratch::UNMERGE
        .remove(&plan.repo.path)
        .await
        .context("could not remove the interrupted unmerge's replay worktree")
}

/// The commit `rev` names.
pub async fn tip_commit(repo: &Path, rev: &str) -> Result<CommitId> {
    git::resolve_commit(repo, rev, git_run).await
}

#[cfg(test)]
mod tests;
