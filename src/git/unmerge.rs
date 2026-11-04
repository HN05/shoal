//! Undoing a local merge into the default branch: the history queries the
//! daemon checks an undo against.
use std::path::Path;

use anyhow::{Result, ensure};

use crate::git::{self, run_isolated as git_run};

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
