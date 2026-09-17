//! Branch allocation and upstream refreshes for a registered repository.
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use uuid::Uuid;

use crate::{
    git::{self, run_isolated as git_run},
    model::{LandPlan, LandedBranch, PulledBranch, Repository},
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
        self.refresh_branch(&repo, &branch, true).await
    }

    /// Validate and refresh a landing while the caller holds the repository Git
    /// gate. The gate must stay held until the tracked execution completes.
    pub async fn prepare_land(&self, selector: &str) -> Result<LandPlan> {
        let workspace = self.workspace(selector).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        ensure!(
            workspace.path.is_dir(),
            "workspace directory is missing: {}",
            workspace.path.display()
        );
        self.verify_worktree(&workspace).await?;
        let default = crate::default_branch::resolve(&repo.path, true).await?;
        let branch = workspace.branch.as_str();
        ensure!(
            branch != default,
            "workspace {} is on the default branch {default}; nothing to land",
            workspace.name
        );
        clean_branch(&workspace.path, branch)
            .await
            .with_context(|| format!("workspace {} is not ready to land", workspace.name))?;
        let source = git_run(
            &workspace.path,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{branch}^{{commit}}"),
            ],
        )
        .await?
        .trim()
        .to_owned();
        if let Some(checkout) = self.managed_checkout(&repo, &default).await? {
            bail!(
                "{default} is checked out in workspace {checkout}; land needs it outside managed workspaces"
            );
        }
        let default_ref = format!("refs/heads/{default}");
        let default_refresh = if upstream(&repo, &default_ref).await?.is_none() {
            let commit = git_run(&repo.path, &["rev-parse", "--verify", &default_ref])
                .await
                .with_context(|| format!("repository has no local {default} branch"))?
                .trim()
                .to_owned();
            PulledBranch {
                branch: default.clone(),
                repository_id: repo.id.clone(),
                updated: false,
                previous_commit: commit.clone(),
                commit,
                skipped: Some(format!(
                    "{default} has no upstream; landing on its local state"
                )),
            }
        } else {
            self.refresh_branch(&repo, &default, false)
                .await
                .with_context(|| format!("could not refresh {default} before landing"))?
        };
        Ok(LandPlan {
            workspace,
            repo,
            source,
            default_refresh,
        })
    }

    /// Fast-forward a local merge source from its upstream so `shoal merge`
    /// imports current work. A source without an upstream, or checked out in
    /// a managed workspace whose branch must stay untouched, is left as it is.
    pub async fn refresh_merge_source(&self, selector: &str, branch: &str) -> Result<PulledBranch> {
        let workspace = self.workspace(selector).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        let gate = self.git_gate(&repo.id).await;
        let _guard = gate.lock().await;
        let local_ref = format!("refs/heads/{branch}");
        git_run(&repo.path, &["check-ref-format", &local_ref])
            .await
            .context("invalid source branch name")?;
        let commit = git_run(&repo.path, &["rev-parse", "--verify", &local_ref])
            .await
            .with_context(|| format!("local source branch does not exist: {branch}"))?
            .trim()
            .to_owned();
        let skipped = if upstream(&repo, &local_ref).await?.is_none() {
            Some(format!("{branch} has no upstream; merging its local state"))
        } else {
            self.managed_checkout(&repo, branch).await?.map(|checkout| {
                format!("{branch} is checked out in workspace {checkout}; merging its local state")
            })
        };
        match skipped {
            Some(skipped) => Ok(PulledBranch {
                branch: branch.into(),
                repository_id: repo.id.clone(),
                updated: false,
                previous_commit: commit.clone(),
                commit,
                skipped: Some(skipped),
            }),
            None => self.refresh_branch(&repo, branch, false).await,
        }
    }

    /// The managed workspace that has `branch` checked out, if any.
    async fn managed_checkout(&self, repo: &Repository, branch: &str) -> Result<Option<String>> {
        let Some(checkout) = git::worktrees(&repo.path)
            .await?
            .into_iter()
            .find(|tree| tree.is_branch(branch))
        else {
            return Ok(None);
        };
        let Ok(path) = std::fs::canonicalize(&checkout.path) else {
            return Ok(None);
        };
        Ok(self
            .list_workspaces()
            .await?
            .into_iter()
            .find(|w| std::fs::canonicalize(&w.path).is_ok_and(|managed| managed == path))
            .map(|w| w.name))
    }

    /// Fast-forward a local branch from its upstream. The caller holds the
    /// repository's Git gate through any subsequent creation.
    pub(crate) async fn refresh_branch(
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
        let upstream = upstream(repo, &local_ref).await?;
        if allow_local_only
            && upstream.is_none()
            && git_run(&repo.path, &["remote"]).await?.trim().is_empty()
        {
            return Ok(PulledBranch {
                branch: branch.into(),
                repository_id: repo.id.clone(),
                updated: false,
                commit: previous_commit.clone(),
                previous_commit,
                skipped: Some(format!(
                    "{branch} has no upstream and the repository has no remotes; nothing to pull"
                )),
            });
        }
        let (remote, reference) = upstream.with_context(|| {
            format!(
                "{branch} has no branch upstream; configure it with git branch --set-upstream-to=<remote>/{branch} {branch}"
            )
        })?;
        let (remote, reference) = (remote.as_str(), reference.as_str());
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
                "{branch} changed during fetch; retry"
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
            skipped: None,
        })
    }
}

/// The `(remote, refs/heads/...)` upstream of a local branch, if configured.
async fn upstream(repo: &Repository, local_ref: &str) -> Result<Option<(String, String)>> {
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
    Ok((!remote.is_empty() && reference.starts_with("refs/heads/"))
        .then(|| (remote.to_owned(), reference.to_owned())))
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

/// Whether `ancestor` is reachable from `descendant`.
async fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let mut command = git::isolated_command(repo);
    command.args(["merge-base", "--is-ancestor", ancestor, descendant]);
    let output = command.output().await.context("compare commits")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "git merge-base failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

/// A branch's checkout must be exactly on that branch and clean before Shoal
/// moves it.
async fn clean_branch(path: &Path, branch: &str) -> Result<()> {
    // A detached HEAD has no symbolic ref; that is the same refusal.
    let head = git_run(path, &["symbolic-ref", "--quiet", "HEAD"])
        .await
        .unwrap_or_default();
    ensure!(
        head == format!("refs/heads/{branch}\n"),
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
pub async fn finish_land(plan: LandPlan) -> Result<LandedBranch> {
    let LandPlan {
        workspace,
        repo,
        source,
        default_refresh,
    } = plan;
    let branch = workspace.branch.as_str();
    let default = default_refresh.branch.clone();
    let default_ref = format!("refs/heads/{default}");
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
    };
    if is_ancestor(&repo.path, &source, &previous).await? {
        return Ok(landed(previous.clone(), false, true));
    }
    let fast_forward = is_ancestor(&repo.path, &previous, &source).await?;
    let checkout = git::worktrees(&repo.path)
        .await?
        .into_iter()
        .find(|tree| tree.is_branch(&default))
        .map(|tree| tree.path);
    match checkout {
        Some(checkout) => {
            clean_branch(&checkout, &default).await?;
            let mut command = git::isolated_command(&checkout);
            command.args([
                "-c",
                "submodule.recurse=false",
                "merge",
                "--ff",
                "--no-squash",
                "--no-edit",
                "--no-stat",
                "--no-autostash",
                "--no-overwrite-ignore",
                "-m",
                &format!("Merge branch '{branch}'"),
                "--",
                &source,
            ]);
            let output = command
                .output()
                .await
                .context("merge into default branch")?;
            if !output.status.success() {
                // Leave the checkout as it was; conflicts belong in the workspace.
                let _ = git_run(&checkout, &["merge", "--abort"]).await;
                bail!(
                    "merging {branch} into {default} failed; run shoal merge {default} in the workspace, resolve conflicts there, and retry\n{}{}",
                    String::from_utf8_lossy(&output.stdout).trim_end(),
                    String::from_utf8_lossy(&output.stderr).trim_end()
                );
            }
        }
        None => {
            ensure!(
                fast_forward,
                "{default} is not checked out and {branch} does not fast-forward it; run shoal merge {default} in the workspace, then retry"
            );
            // Native fetch refuses a checked-out destination and a non-fast-forward.
            git_run(
                &repo.path,
                &[
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--no-write-fetch-head",
                    ".",
                    &format!("{source}:{default_ref}"),
                ],
            )
            .await?;
        }
    }
    let commit = git_run(&repo.path, &["rev-parse", "--verify", &default_ref])
        .await?
        .trim()
        .to_owned();
    Ok(landed(commit, true, fast_forward))
}

#[cfg(test)]
mod tests;
