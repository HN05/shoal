//! Branch naming, upstream lookup and landing merges for a registered repository.
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use crate::{
    git::{self, run_isolated as git_run, worktrunk},
    model::{LandPlan, LandedBranch, Repository},
};

/// The `(remote, refs/heads/...)` upstream of a local branch, if configured.
pub(crate) async fn upstream(
    repo: &Repository,
    local_ref: &str,
) -> Result<Option<(String, String)>> {
    let upstream = git_run(
        &repo.path,
        &[
            "for-each-ref",
            "--format=%(upstream:remotename)%00%(upstream:remoteref)",
            local_ref,
        ],
    )
    .await?;
    let (remote, reference) = upstream
        .trim_end_matches('\n')
        .split_once('\0')
        .context("invalid Git upstream")?;
    Ok(
        (!remote.is_empty() && git::strip_local(reference).is_some())
            .then(|| (remote.to_owned(), reference.to_owned())),
    )
}

/// Suffix conflicting components with `-2`, `-3`, ... A branch at an ancestor
/// blocks all its descendants, so that component is suffixed rather than
/// repeatedly suffixing an unreachable leaf.
pub(crate) fn allocate_branch(name: &str, taken: &[String]) -> String {
    let mut prefix = String::new();
    let mut components = name.split('/').peekable();
    while let Some(component) = components.next() {
        let base = format!("{prefix}{component}");
        let mut candidate = base.clone();
        let mut suffix = 2_u64;
        let last = components.peek().is_none();
        while (last && worktrunk::is_reserved_branch_name(&candidate))
            || taken.iter().any(|existing| {
                existing == &candidate || (last && existing.starts_with(&format!("{candidate}/")))
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
    prefix
}

/// A branch's checkout must be exactly on that branch and clean before Shoal
/// moves it.
pub(crate) async fn clean_branch(path: &Path, branch: &str) -> Result<()> {
    // A detached HEAD has no symbolic ref; that is the same refusal.
    let head = git::head_branch(path, true, git_run)
        .await
        .unwrap_or_default();
    ensure!(
        head.as_deref() == Some(branch),
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

/// Executed only by the landing worker, inside the tracked process group.
/// With `push`, the default branch then goes to its upstream; a missing
/// upstream is refused before anything merges.
pub async fn finish_land(plan: LandPlan, push: bool) -> Result<LandedBranch> {
    let default = plan.default_refresh.branch.clone();
    let target = if push {
        let target = upstream(&plan.repo, &git::local_ref(&default)).await?;
        Some(target.with_context(|| format!("{default} has no upstream to push to"))?)
    } else {
        None
    };
    let repo = plan.repo.path.clone();
    let mut landed = merge_land(plan).await?;
    if let Some((remote, reference)) = target {
        git_run(
            &repo,
            &[
                "push",
                "--",
                &remote,
                &format!("{}:{reference}", landed.commit),
            ],
        )
        .await
        .with_context(|| {
            format!(
                "{} landed in {default} ({}), but pushing {default} to {remote} failed",
                landed.branch, landed.commit
            )
        })?;
        landed.pushed_to = Some(remote);
    }
    Ok(landed)
}

async fn merge_land(plan: LandPlan) -> Result<LandedBranch> {
    let LandPlan {
        workspace,
        repo,
        source,
        checkout,
        merge_temporaries: _,
        default_refresh,
    } = plan;
    let branch = workspace.branch.as_str();
    let default = default_refresh.branch.clone();
    let default_ref = git::local_ref(&default);
    clean_branch(&workspace.path, branch).await?;
    ensure!(
        git_run(&workspace.path, &["rev-parse", "HEAD"])
            .await?
            .trim()
            == source,
        "workspace branch changed before landing; retry"
    );
    ensure!(
        git_run(&repo.path, &["rev-parse", &default_ref])
            .await?
            .trim()
            == default_refresh.commit,
        "default branch changed before landing; retry"
    );
    let previous = default_refresh.commit.clone();
    let landed = |commit: String, updated: bool, fast_forward: bool| LandedBranch {
        workspace_id: workspace.id.clone(),
        repository_id: repo.id.clone(),
        branch: branch.to_owned(),
        default_branch: default.clone(),
        previous_commit: previous.clone(),
        commit,
        updated,
        fast_forward,
        default_refresh,
        pushed_to: None,
    };
    if git::is_ancestor(&repo.path, &source, &previous, git::isolated_command).await? {
        return Ok(landed(previous.clone(), false, true));
    }
    let fast_forward =
        git::is_ancestor(&repo.path, &previous, &source, git::isolated_command).await?;
    match checkout {
        Some(checkout) => {
            clean_branch(&checkout, &default).await?;
            let output = git::merge_commit(&checkout, branch, &source)
                .output()
                .await
                .context("merge into default branch")?;
            if !output.status.success() {
                // Leave the checkout as it was; conflicts belong in the workspace.
                let _ = git_run(&checkout, &["merge", "--abort"]).await;
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
            git::fast_forward_local(&repo.path, &source, &default_ref).await?;
        }
    }
    let commit = git_run(&repo.path, &["rev-parse", "--verify", &default_ref])
        .await?
        .trim()
        .to_owned();
    Ok(landed(commit, true, fast_forward))
}

/// The wrapper runs rollback only after stopping the merge process group. Keep
/// a completed merge, and refuse to reset a checkout whose branch/HEAD changed.
pub async fn rollback_land(plan: &LandPlan) -> Result<()> {
    let Some(checkout) = &plan.checkout else {
        return Ok(());
    };
    let branch = &plan.default_refresh.branch;
    ensure!(
        git::head_branch(checkout, true, git_run).await?.as_deref() == Some(branch),
        "landing checkout changed branches; recover it manually"
    );
    let head = git_run(checkout, &["rev-parse", "HEAD"]).await?;
    if head.trim() != plan.default_refresh.commit {
        return Ok(());
    }
    // --merge restores the index and merge state without discarding unrelated
    // unstaged edits. Git refuses an index lock; never remove someone else's lock.
    git_run(
        checkout,
        &["reset", "--merge", &plan.default_refresh.commit],
    )
    .await
    .context("could not roll back interrupted landing; inspect the default checkout")?;
    // Git's external merge drivers use these temporary inputs in the checkout.
    // A signal can bypass Git's normal unlink; preserve any that predated us.
    for path in merge_temporaries(checkout)? {
        if !plan.merge_temporaries.contains(&path) {
            std::fs::remove_file(&path).context("remove interrupted Git merge input")?;
        }
    }
    Ok(())
}

pub(crate) fn merge_temporaries(checkout: &Path) -> Result<Vec<std::path::PathBuf>> {
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
