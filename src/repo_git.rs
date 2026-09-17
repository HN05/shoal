//! Branch allocation and default-branch refreshes for a registered repository.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use uuid::Uuid;

use crate::{
    git::{self, run_isolated as git_run},
    model::{PulledBranch, Repository},
    workspace::Manager,
};

impl Manager {
    /// The requested branch name, or the nearest free `-2`, `-3`, ... variant.
    /// Called while holding the repository's Git gate, through worktree creation.
    pub(crate) async fn available_branch(&self, repo: &Repository, name: &str) -> Result<String> {
        let refs = git::run(
            &repo.path,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/",
                "refs/remotes/",
            ],
        )
        .await?;
        let mut taken: Vec<String> = refs
            .lines()
            .filter_map(|reference| {
                reference.strip_prefix("refs/heads/").or_else(|| {
                    reference
                        .strip_prefix("refs/remotes/")?
                        .split_once('/')
                        .map(|(_, branch)| branch)
                })
            })
            .map(str::to_owned)
            .collect();
        // Failed/preparing workspaces still own their recorded branch name.
        taken.extend(
            self.list_workspaces()
                .await?
                .into_iter()
                .filter(|workspace| workspace.repository_id == repo.id)
                .map(|workspace| workspace.branch),
        );
        Ok(allocate_branch(name, &taken))
    }

    pub async fn pull_default_branch(&self, selector: &str) -> Result<PulledBranch> {
        let workspace = self.workspace(selector).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        let gate = self.git_gate(&repo.id).await;
        let _guard = gate.lock().await;
        let branch = crate::default_branch::resolve(&repo.path, true).await?;
        self.refresh_default_branch(&repo, &branch, false).await
    }

    /// Fast-forward the local default branch from its upstream. The caller
    /// holds the repository's Git gate through any subsequent creation.
    pub(crate) async fn refresh_default_branch(
        &self,
        repo: &Repository,
        branch: &str,
        allow_local_only: bool,
    ) -> Result<PulledBranch> {
        let local_ref = format!("refs/heads/{branch}");
        let previous_commit = git_run(&repo.path, &["rev-parse", "--verify", &local_ref])
            .await
            .with_context(|| format!("repository has no local {branch} branch"))?
            .trim()
            .to_owned();
        let upstream = git_run(
            &repo.path,
            &[
                "for-each-ref",
                "--format=%(upstream:remotename)%00%(upstream:remoteref)",
                &local_ref,
            ],
        )
        .await?;
        let (remote, reference) = upstream
            .trim_end_matches('\n')
            .split_once('\0')
            .context("invalid Git upstream")?;
        if allow_local_only
            && remote.is_empty()
            && reference.is_empty()
            && git_run(&repo.path, &["remote"]).await?.trim().is_empty()
        {
            return Ok(PulledBranch {
                branch: branch.into(),
                repository_id: repo.id.clone(),
                updated: false,
                commit: previous_commit.clone(),
                previous_commit,
            });
        }
        ensure!(
            !remote.is_empty() && reference.starts_with("refs/heads/"),
            "{branch} has no branch upstream; configure it with git branch --set-upstream-to=<remote>/{branch} {branch}"
        );
        let checkout = git::worktrees(&repo.path)
            .await?
            .into_iter()
            .find(|tree| tree.is_branch(branch))
            .map(|tree| tree.path);
        if let Some(checkout) = &checkout {
            let path = std::fs::canonicalize(checkout)?;
            ensure!(
                !self.list_workspaces().await?.iter().any(|w| {
                    std::fs::canonicalize(&w.path).is_ok_and(|managed| managed == path)
                }),
                "{branch} is checked out in a managed workspace; switch that workspace back to its own branch first"
            );
            clean_branch(checkout, branch).await?;
        }

        // A private fetch ref avoids races with unrelated fetches overwriting FETCH_HEAD.
        let fetched = format!("refs/shoal/pull/{}", Uuid::new_v4());
        let result = async {
            git_run(
                &repo.path,
                &[
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--no-write-fetch-head",
                    "--",
                    remote,
                    &format!("{reference}:{fetched}"),
                ],
            )
            .await?;
            let commit = git_run(&repo.path, &["rev-parse", "--verify", &fetched]).await?;
            let commit = commit.trim();
            ensure!(
                git_run(&repo.path, &["rev-parse", &local_ref])
                    .await?
                    .trim()
                    == previous_commit,
                "{branch} changed during fetch; retry shoal pull"
            );
            // Like pull --ff-only, an already-ahead default branch stays untouched.
            if git_run(
                &repo.path,
                &["merge-base", "--is-ancestor", commit, &previous_commit],
            )
            .await
            .is_ok()
            {
                return Ok(previous_commit.clone());
            }
            git_run(
                &repo.path,
                &["merge-base", "--is-ancestor", &previous_commit, commit],
            )
            .await
            .with_context(|| {
                format!(
                    "{branch} and its upstream have diverged; resolve this manually before pulling"
                )
            })?;
            if let Some(checkout) = &checkout {
                clean_branch(checkout, branch).await?;
                git_run(
                    checkout,
                    &[
                        "-c",
                        "submodule.recurse=false",
                        "merge",
                        "--ff-only",
                        "--no-edit",
                        "--no-stat",
                        "--no-overwrite-ignore",
                        commit,
                    ],
                )
                .await?;
            } else {
                // Native fetch refuses a checked-out destination and a non-fast-forward.
                // This also guards against the default branch becoming checked out since discovery.
                git_run(
                    &repo.path,
                    &[
                        "fetch",
                        "--no-tags",
                        "--no-recurse-submodules",
                        "--no-write-fetch-head",
                        ".",
                        &format!("{commit}:{local_ref}"),
                    ],
                )
                .await?;
            }
            Ok(commit.to_owned())
        }
        .await;
        let cleanup = git_run(&repo.path, &["update-ref", "-d", &fetched]).await;
        let commit = result?;
        cleanup.context("could not remove temporary pull ref")?;
        Ok(PulledBranch {
            branch: branch.into(),
            repository_id: repo.id.clone(),
            updated: commit != previous_commit,
            previous_commit,
            commit,
        })
    }
}

/// Suffix conflicting components with `-2`, `-3`, ... A branch at an ancestor
/// blocks all its descendants, so that component is suffixed rather than
/// repeatedly suffixing an unreachable leaf.
fn allocate_branch(name: &str, taken: &[String]) -> String {
    let mut prefix = String::new();
    let mut components = name.split('/').peekable();
    while let Some(component) = components.next() {
        let base = format!("{prefix}{component}");
        let mut candidate = base.clone();
        let mut suffix = 2_u64;
        let last = components.peek().is_none();
        while (last && is_reserved_leaf(&candidate))
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

/// Worktrunk interprets `@` as the current branch, even with --create; Git
/// worktree add treats full hex object IDs as commits.
fn is_reserved_leaf(candidate: &str) -> bool {
    matches!(candidate, "HEAD" | "@")
        || (matches!(candidate.len(), 40 | 64) && candidate.bytes().all(|c| c.is_ascii_hexdigit()))
}

/// The default branch's checkout must be exactly on that branch and clean.
async fn clean_branch(path: &Path, branch: &str) -> Result<()> {
    ensure!(
        git_run(path, &["symbolic-ref", "--quiet", "HEAD"]).await?
            == format!("refs/heads/{branch}\n"),
        "{branch} checkout changed branches; retry shoal pull"
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
        "{branch} checkout has uncommitted or untracked changes; clean it before pulling"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
