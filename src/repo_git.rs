use std::path::Path;

use anyhow::{Context, Result, ensure};
use tokio::process::Command;
use uuid::Uuid;

use crate::{
    model::{PulledBranch, Repository},
    workspace::Manager,
    worktrunk,
};

impl Manager {
    // Called while holding the repository's Git gate, through worktree creation.
    pub(crate) async fn available_branch(&self, repo: &Repository, name: &str) -> Result<String> {
        let refs = worktrunk::git(
            &repo.path,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/",
                "refs/remotes/",
            ],
        )
        .await?;
        let mut names: Vec<String> = refs
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
        names.extend(
            self.list()
                .await?
                .into_iter()
                .filter(|workspace| workspace.repository_id == repo.id)
                .map(|workspace| workspace.branch),
        );
        let mut prefix = String::new();
        let mut components = name.split('/').peekable();
        while let Some(component) = components.next() {
            let base = format!("{prefix}{component}");
            let mut candidate = base.clone();
            let mut suffix = 2_u64;
            let last = components.peek().is_none();
            // A branch at an ancestor blocks all its descendants. Suffix that
            // component rather than repeatedly suffixing an unreachable leaf.
            // Worktrunk interprets @ as the current branch, even with --create;
            // Git worktree add treats full hex object IDs as commits.
            while (last
                && (matches!(candidate.as_str(), "HEAD" | "@")
                    || (matches!(candidate.len(), 40 | 64)
                        && candidate.bytes().all(|c| c.is_ascii_hexdigit()))))
                || names.iter().any(|existing| {
                    existing == &candidate
                        || (last && existing.starts_with(&format!("{candidate}/")))
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
        Ok(prefix)
    }

    pub async fn pull_default_branch(&self, selector: String) -> Result<PulledBranch> {
        let workspace = self.get(selector).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        let gate = self.git_gate(&repo.id).await;
        let _guard = gate.lock().await;
        let branch = crate::default_branch::resolve(&repo.path, true).await?;
        self.refresh_default_branch(&repo, &branch, false).await
    }

    // The caller holds the repository's Git gate through any subsequent creation.
    pub(crate) async fn refresh_default_branch(
        &self,
        repo: &Repository,
        branch: &str,
        allow_local_only: bool,
    ) -> Result<PulledBranch> {
        let local_ref = format!("refs/heads/{branch}");
        let previous_commit = git(&repo.path, &["rev-parse", "--verify", &local_ref])
            .await
            .with_context(|| format!("repository has no local {branch} branch"))?
            .trim()
            .to_owned();
        let upstream = git(
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
            && git(&repo.path, &["remote"]).await?.trim().is_empty()
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
        let trees = git(&repo.path, &["worktree", "list", "--porcelain", "-z"]).await?;
        let checkout = trees.split("\0\0").find_map(|record| {
            record
                .split('\0')
                .any(|field| field == format!("branch {local_ref}"))
                .then(|| {
                    record
                        .split('\0')
                        .find_map(|field| field.strip_prefix("worktree "))
                })
                .flatten()
        });
        if let Some(checkout) = checkout {
            let path = std::fs::canonicalize(checkout)?;
            ensure!(
                !self.list().await?.iter().any(|w| {
                    std::fs::canonicalize(&w.path).is_ok_and(|managed| managed == path)
                }),
                "{branch} is checked out in a managed workspace; switch that workspace back to its own branch first"
            );
            clean_branch(Path::new(checkout), branch).await?;
        }

        // A private fetch ref avoids races with unrelated fetches overwriting FETCH_HEAD.
        let fetched = format!("refs/shoal/pull/{}", Uuid::new_v4());
        let result = async {
            git(
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
            let commit = git(&repo.path, &["rev-parse", "--verify", &fetched]).await?;
            let commit = commit.trim();
            ensure!(
                git(&repo.path, &["rev-parse", &local_ref]).await?.trim() == previous_commit,
                "{branch} changed during fetch; retry shoal pull"
            );
            // Like pull --ff-only, an already-ahead default branch stays untouched.
            if git(
                &repo.path,
                &["merge-base", "--is-ancestor", commit, &previous_commit],
            )
            .await
            .is_ok()
            {
                return Ok(previous_commit.clone());
            }
            git(
                &repo.path,
                &["merge-base", "--is-ancestor", &previous_commit, commit],
            )
            .await
            .with_context(|| {
                format!(
                    "{branch} and its upstream have diverged; resolve this manually before pulling"
                )
            })?;
            if let Some(checkout) = checkout {
                clean_branch(Path::new(checkout), branch).await?;
                git(
                    Path::new(checkout),
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
                git(
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
        let cleanup = git(&repo.path, &["update-ref", "-d", &fetched]).await;
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

async fn clean_branch(path: &Path, branch: &str) -> Result<()> {
    ensure!(
        git(path, &["symbolic-ref", "--quiet", "HEAD"]).await? == format!("refs/heads/{branch}\n"),
        "{branch} checkout changed branches; retry shoal pull"
    );
    ensure!(
        git(
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

async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    worktrunk::run(command).await
}

#[cfg(test)]
mod tests;
