//! Undoing a local merge into the default branch: the history queries the
//! daemon checks an undo against, and the moves the unmerge worker makes.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::{
    fsutil::{self, off_runtime},
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

/// The commit a merge of `source` onto `previous` left the default branch
/// at, found in `tip`'s first-parent history: `source` itself after a
/// fast-forward, else the merge commit of the two. `None` when that history
/// shows no such merge, as after one that merged nothing.
pub async fn merge_result(
    repo: &Path,
    tip: &str,
    previous: &str,
    source: &str,
) -> Result<Option<String>> {
    // A fast-forward needs `source` on the tip's first-parent history and
    // `previous` among its ancestors, though not on its first parents: a
    // branch that merged the default branch in holds it as a second parent.
    let fast_forward = source != previous
        && git::is_ancestor(repo, previous, source, git::isolated_command).await?
        && first_parent_chain(repo, tip, source).await?.is_some();
    if fast_forward {
        return Ok(Some(source.to_owned()));
    }
    let Some(chain) = first_parent_chain(repo, tip, previous).await? else {
        return Ok(None);
    };
    let Some(first) = chain.last() else {
        return Ok(None);
    };
    let merged = merged_parents(repo, first).await?;
    Ok(merged
        .iter()
        .any(|parent| parent == source)
        .then(|| first.clone()))
}

/// Whether the upstream of `default`, as the last fetch saw it, holds
/// `commit`, so others may have it.
pub async fn pushed(repo: &Path, default: &str, commit: &str) -> Result<bool> {
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
        !pushed(repo, default, &plan.result).await?,
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
pub(crate) async fn checkout_at(checkout: &Path, branch: &str, commit: &str) -> Result<bool> {
    let head = git::head_branch(checkout, true, git_run)
        .await
        .unwrap_or_default();
    Ok(head.as_deref() == Some(branch) && tip_commit(checkout, "HEAD").await? == commit)
}

/// What a replay keeps of `commit`, which a rebase carries over to the
/// commit it makes in its place: its author, author date and message, the
/// commits it merged, and the lines it changes from its first parent, by
/// the patch ID of a diff without context, which taking out an earlier
/// merge's nearby changes leaves as it was.
pub async fn replay_identity(repo: &Path, commit: &str) -> Result<String> {
    let metadata = git_run(
        repo,
        &[
            "show",
            "-s",
            "--format=%an%x00%ae%x00%ad%x00%B",
            commit,
            "--",
        ],
    )
    .await?;
    let merged = merged_parents(repo, commit).await?.join(" ");
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
async fn move_default(plan: &UnmergePlan, target: &str) -> Result<()> {
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
            target,
            &plan.tip,
        ];
        git_run(&plan.repo.path, &args)
            .await
            .with_context(|| format!("move {default} to {target}"))?;
        return Ok(());
    };
    clean_branch(checkout, default).await?;
    let collisions = git::checkpoint::ignored_collisions(checkout, target, target).await?;
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
    git_run(checkout, &["reset", "--keep", target])
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
    let worktree = add_replay_worktree(repo, &plan.tip).await?;
    let replayed = replay_in(plan, &worktree).await;
    remove_replay_worktree(repo).await?;
    let (commit, replayed) = replayed?;
    move_default(plan, &commit).await?;
    Ok(replayed)
}

/// Replay the history after the merge in `worktree`, detached at the tip:
/// the commit it makes and how many first-parent commits it replayed.
async fn replay_in(plan: &UnmergePlan, worktree: &Path) -> Result<(String, u64)> {
    let default = &plan.default_branch;
    let output = git::bounded(rebase_command(worktree, &plan.previous, &plan.result))
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
    if holds_commits_of(worktree, &commit, &plan.source, &plan.previous).await? {
        bail!(
            "replaying the commits after {} merged would bring its commits back into {default}, so nothing changed; unmerge the merges that contain them first",
            plan.branch
        );
    }
    let replayed = first_parent_chain(worktree, &commit, &plan.previous)
        .await?
        .map_or(0, |chain| chain.len());
    Ok((commit, replayed as u64))
}

/// The owner a replay directory is marked with.
const REPLAY_OWNER: &str = "shoal unmerge";

/// The directory in `repo`'s Git directory that holds a replay's worktree:
/// one place, which the repository Git gate gives one unmerge at a time, so
/// rollback finds what an interrupted replay left.
async fn replay_directory(repo: &Path) -> Result<PathBuf> {
    let path = git::metadata_path(repo, "shoal-unmerge", git_run).await?;
    Ok(repo.join(path.trim()))
}

/// Create a worktree detached at `tip` to replay in, in a directory marked
/// as Shoal's, after removing one an earlier replay left; its path.
async fn add_replay_worktree(repo: &Path, tip: &str) -> Result<PathBuf> {
    remove_replay_worktree(repo).await?;
    let directory = replay_directory(repo).await?;
    let marked = directory.clone();
    off_runtime::run(move || {
        Ok(fsutil::marked_dirs::create_marked_dir(
            &marked,
            REPLAY_OWNER,
        )?)
    })
    .await
    .with_context(|| format!("create {}", directory.display()))?;
    ensure_replay_directory(&directory).await?;
    let worktree = directory.join("replay");
    let path = worktree.to_string_lossy();
    git_run(
        repo,
        &["worktree", "add", "--quiet", "--detach", &path, tip],
    )
    .await
    .context("create the worktree the replay runs in")?;
    Ok(worktree)
}

/// Remove the replay worktree an unmerge left and Git's record of it, if
/// any; a directory Shoal did not mark as its own is refused, not deleted.
async fn remove_replay_worktree(repo: &Path) -> Result<()> {
    let directory = replay_directory(repo).await?;
    if !off_runtime::exists(&directory).await? {
        return Ok(());
    }
    ensure_replay_directory(&directory).await?;
    let worktree = directory.join("replay");
    if off_runtime::exists(&worktree).await? {
        let removed = worktree.clone();
        off_runtime::run(move || Ok(std::fs::remove_dir_all(&removed)?))
            .await
            .with_context(|| format!("remove {}", worktree.display()))?;
    }
    git_run(repo, &["worktree", "prune"]).await?;
    Ok(())
}

/// Refuse a replay directory Shoal did not create.
async fn ensure_replay_directory(directory: &Path) -> Result<()> {
    let checked = directory.to_owned();
    let marked = off_runtime::run(move || {
        Ok(fsutil::marked_dirs::check_marked_dir(
            &checked,
            REPLAY_OWNER,
        )?)
    })
    .await
    .with_context(|| format!("read {}", directory.display()))?;
    ensure!(
        marked,
        "{} exists and Shoal did not create it, so the unmerge does not replay there; remove it to unmerge",
        directory.display()
    );
    Ok(())
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
    if git::ref_exists(&plan.repo.path, &reference, git::isolated_command).await? {
        return Ok(false);
    }
    git::branch::create(&plan.repo.path, &plan.branch, &plan.source).await?;
    Ok(true)
}

/// The wrapper runs rollback only after stopping the unmerge process group:
/// a replay it interrupted ran in a worktree of its own, which goes, and
/// the default branch moves only once the replay is done.
pub async fn rollback_unmerge(plan: &UnmergePlan) -> Result<()> {
    remove_replay_worktree(&plan.repo.path)
        .await
        .context("could not remove the interrupted unmerge's replay worktree")
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
