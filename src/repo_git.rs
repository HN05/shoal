use std::path::Path;

use anyhow::{Context, Result, ensure};
use tokio::process::Command;
use uuid::Uuid;

use crate::{
    model::{PulledMain, Repository},
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
        let mut candidate = name.to_owned();
        let mut suffix = 2_u64;
        while candidate == "HEAD"
            || names.iter().any(|existing| {
                existing == &candidate || existing.starts_with(&format!("{candidate}/"))
            })
        {
            candidate = format!("{name}-{suffix}");
            suffix += 1;
        }
        Ok(candidate)
    }

    pub async fn pull_main(&self, selector: String) -> Result<PulledMain> {
        let workspace = self.get(selector).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        let gate = self.git_gate(&repo.id).await;
        let _guard = gate.lock().await;
        let previous_commit = git(&repo.path, &["rev-parse", "--verify", "refs/heads/main"])
            .await
            .context("repository has no local main branch")?
            .trim()
            .to_owned();
        let upstream = git(
            &repo.path,
            &[
                "for-each-ref",
                "--format=%(upstream:remotename)%00%(upstream:remoteref)",
                "refs/heads/main",
            ],
        )
        .await?;
        let (remote, reference) = upstream
            .trim()
            .split_once('\0')
            .context("invalid Git upstream")?;
        ensure!(
            !remote.is_empty() && reference.starts_with("refs/heads/"),
            "main has no branch upstream; configure it with git branch --set-upstream-to=<remote>/main main"
        );
        let trees = git(&repo.path, &["worktree", "list", "--porcelain", "-z"]).await?;
        let checkout = trees.split("\0\0").find_map(|record| {
            record
                .split('\0')
                .any(|field| field == "branch refs/heads/main")
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
                "main is checked out in a managed workspace; switch that workspace back to its own branch first"
            );
            clean_main(Path::new(checkout)).await?;
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
                git(&repo.path, &["rev-parse", "refs/heads/main"])
                    .await?
                    .trim()
                    == previous_commit,
                "main changed during fetch; retry shoal pull"
            );
            // Like pull --ff-only, an already-ahead main stays untouched.
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
            .context("main and its upstream have diverged; resolve this manually before pulling")?;
            if let Some(checkout) = checkout {
                clean_main(Path::new(checkout)).await?;
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
                // This also guards against main becoming checked out since discovery.
                git(
                    &repo.path,
                    &[
                        "fetch",
                        "--no-tags",
                        "--no-recurse-submodules",
                        "--no-write-fetch-head",
                        ".",
                        &format!("{commit}:refs/heads/main"),
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
        Ok(PulledMain {
            repository_id: repo.id,
            updated: commit != previous_commit,
            previous_commit,
            commit,
        })
    }
}

async fn clean_main(path: &Path) -> Result<()> {
    ensure!(
        git(path, &["symbolic-ref", "--quiet", "HEAD"])
            .await?
            .trim()
            == "refs/heads/main",
        "main checkout changed branches; retry shoal pull"
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
        "main checkout has uncommitted or untracked changes; clean it before pulling"
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
