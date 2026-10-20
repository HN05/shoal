//! Worktree state and refs a removal inspection compares to decide what to retain.
use std::{
    collections::HashMap,
    path::Path,
    sync::{LazyLock, Mutex, PoisonError},
};

use anyhow::{Context, Result};

use super::run;

pub async fn porcelain(repo: &Path) -> Result<String> {
    run(repo, &["status", "--porcelain", "--untracked-files=normal"]).await
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

/// Commits on HEAD that are reachable from neither a remote nor the given ref.
pub async fn unretained_commits(repo: &Path, retained_ref: Option<&str>) -> Result<u64> {
    let mut args = vec!["rev-list", "--count", "HEAD", "--not", "--remotes"];
    args.extend(retained_ref);
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
        if git_dir.join(marker).try_exists()? {
            return Ok(true);
        }
    }
    Ok(!run(repo, &["ls-files", "--unmerged"]).await?.is_empty())
}

/// The local default branch and its remote-tracking branch, those that exist.
pub async fn default_refs(repo: &Path, name: &str) -> Result<Vec<String>> {
    let Some(local) = local_branch(repo, name).await? else {
        return Ok(Vec::new());
    };
    let tracking = run(repo, &["for-each-ref", "--format=%(upstream)", &local]).await?;
    let tracking = tracking.trim();
    let mut refs = vec![local.clone()];
    if tracking.starts_with(super::REMOTE_REFS)
        && super::ref_exists(repo, tracking, super::command).await?
    {
        refs.push(tracking.to_owned());
    }
    Ok(refs)
}

/// Whether any of `references` retains HEAD, by recording its tree or
/// already having its changes.
pub async fn retains_head(repo: &Path, references: &[String], head_tree: &str) -> Result<bool> {
    for reference in references {
        if records_tree(repo, reference, head_tree).await || merged_into(repo, reference).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `target` already has HEAD's changes: HEAD is its ancestor, a
/// trial merge of HEAD leaves its tree unchanged (squash merges), or every
/// commit since the fork point has an equivalent patch on it (rebase merges).
///
/// The answer depends only on the two commits, so it is remembered by their
/// IDs: cleanup asks again on every pass, and `git cherry` reads every commit
/// since the fork point.
pub async fn merged_into(repo: &Path, target: &str) -> Result<bool> {
    static ANSWERS: LazyLock<Mutex<HashMap<(String, String), bool>>> =
        LazyLock::new(Mutex::default);
    let commits = run(
        repo,
        &[
            "rev-parse",
            "HEAD^{commit}",
            &format!("{target}^{{commit}}"),
        ],
    )
    .await?;
    let mut commits = commits.lines().map(str::to_owned);
    let key = (
        commits.next().context("HEAD has no commit")?,
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
    let cherry = run(repo, &["cherry", target, head]).await?;
    Ok(cherry.lines().all(|line| line.starts_with('-')))
}

/// The tree a clean merge of `head` into `target` would record, or `None`
/// when it conflicts. `git merge-tree` writes objects only.
async fn trial_merge_tree(repo: &Path, head: &str, target: &str) -> Result<Option<String>> {
    let mut command = super::isolated_command_without_submodules(repo);
    command.args(["merge-tree", "--write-tree", "--no-messages", target, head]);
    let output = crate::subprocess::Run::new(command).capture().await;
    if output.as_ref().ok().and_then(|output| output.status.code()) == Some(1) {
        return Ok(None);
    }
    let stdout = crate::subprocess::checked_output("git merge-tree", output)
        .context("trial merge; Git 2.38 or newer is required")?;
    Ok(Some(stdout.trim().to_owned()))
}

/// Whether HEAD's upstream records the tree; a missing upstream does not.
pub async fn upstream_records_tree(repo: &Path, head_tree: &str) -> bool {
    records_tree(repo, "@{upstream}", head_tree).await
}

async fn records_tree(repo: &Path, revision: &str, tree: &str) -> bool {
    super::verify_revision(repo, &format!("{revision}^{{tree}}"), run)
        .await
        .is_ok_and(|other| other == tree)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    use crate::test_support::{commit, git, repository};

    #[tokio::test]
    async fn status_leaves_a_stale_index_unwritten() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path(), "repo");
        let tracked = fs::File::options()
            .write(true)
            .open(repo.join("tracked"))
            .unwrap();
        tracked
            .set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        let index = fs::read(repo.join(".git/index")).unwrap();

        assert_eq!(super::porcelain(&repo).await.unwrap(), "");
        assert_eq!(super::changed_files(&repo).await.unwrap(), "");
        assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index);
    }

    #[tokio::test]
    async fn merged_into_sees_merges_squashes_and_rebases() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path(), "repo");
        git(&repo, &["checkout", "-q", "-b", "topic"]);
        for file in ["one", "two"] {
            fs::write(repo.join(file), format!("{file}\n")).unwrap();
            commit(&repo, file);
        }
        let merged = async |target: &str| super::merged_into(&repo, target).await.unwrap();
        assert!(!merged("main").await);

        // Squash: one commit on main with the topic's combined changes,
        // followed by unrelated work.
        git(&repo, &["checkout", "-q", "-b", "squashed", "main"]);
        git(&repo, &["merge", "-q", "--squash", "topic"]);
        git(&repo, &["commit", "-q", "-m", "squash"]);
        fs::write(repo.join("later"), "later\n").unwrap();
        commit(&repo, "later");
        // Rebase: equivalent patches on top of other work.
        git(&repo, &["checkout", "-q", "-b", "rebased", "main"]);
        fs::write(repo.join("other"), "other\n").unwrap();
        commit(&repo, "other");
        git(&repo, &["cherry-pick", "main..topic"]);
        git(&repo, &["checkout", "-q", "topic"]);
        assert!(merged("squashed").await);
        assert!(merged("rebased").await);
        assert!(merged("topic").await);

        // A later commit is not on any of them.
        fs::write(repo.join("three"), "three\n").unwrap();
        commit(&repo, "three");
        for target in ["squashed", "rebased", "main"] {
            assert!(!merged(target).await, "{target}");
        }
    }
}
