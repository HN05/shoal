//! Merge preparation: validate a workspace branch, say what merging it does,
//! and refresh the default branch before the merge worker merges it.
mod approval;

use anyhow::{Context, Result, bail, ensure};

use super::{Manager, branches::UpstreamPolicy};
use crate::{
    fsutil::off_runtime,
    git::{
        self,
        default_branch::DefaultBranchLookup,
        repo::{clean_branch, merge_temporaries, unfinished_operation, upstream},
        run_isolated as git_run,
    },
    model::{
        MergeConfirmation, MergeOptions, MergePlan, MergePreview, MergePush, PulledBranch,
        Repository, Workspace,
    },
};

/// A workspace branch that passed every check a merge makes before it
/// changes anything.
struct MergeSource {
    workspace: Workspace,
    branch: String,
    repo: Repository,
    default: String,
    /// The branch's commit that merges.
    source: String,
    push: Option<MergePush>,
}

impl Manager {
    /// What merging the workspace `selector` names does, after the checks the
    /// merge makes before it changes anything; with `push`, the merge pushes
    /// the default branch afterwards.
    pub async fn preview_merge(&self, selector: &str, push: bool) -> Result<MergePreview> {
        let merge = self.validate_merge(selector, push).await?;
        let default_commit = local_commit(&merge.repo, &merge.default).await?;
        self.merge_preview(
            &merge.workspace,
            &merge.branch,
            &merge.source,
            &merge.default,
            &default_commit,
            merge.push.as_ref(),
        )
        .await
    }

    /// Validate and refresh a merge that runs as `options` say while the
    /// caller holds the repository Git gate. The gate must stay held until the
    /// tracked execution completes.
    pub async fn prepare_merge(&self, selector: &str, options: &MergeOptions) -> Result<MergePlan> {
        let merge = self.validate_merge(selector, options.push).await?;
        if let Some(confirmed) = &options.confirmed {
            ensure_confirmed(&merge, confirmed).await?;
        }
        let MergeSource {
            workspace,
            branch,
            repo,
            default,
            source,
            push,
        } = merge;
        let default_refresh = self.refresh_default(&repo, &default).await?;
        let checkout = git::checkout_of(&repo.path, &default, git::run)
            .await?
            .map(|tree| tree.path);
        if let Some(checkout) = &checkout {
            clean_branch(checkout, &default).await?;
            if let Some(marker) = unfinished_operation(checkout).await? {
                bail!("{default} has an unfinished Git operation ({marker})");
            }
        }
        let merge_temporaries = checkout
            .as_deref()
            .map(merge_temporaries)
            .transpose()?
            .unwrap_or_default();
        Ok(MergePlan {
            workspace,
            branch,
            repo,
            source,
            checkout,
            merge_temporaries,
            default_refresh,
            push,
        })
    }

    /// What merging `plan` does, for the approval of a scoped merge.
    async fn plan_preview(&self, plan: &MergePlan) -> Result<MergePreview> {
        self.merge_preview(
            &plan.workspace,
            &plan.branch,
            &plan.source,
            &plan.default_refresh.branch,
            &plan.default_refresh.commit,
            plan.push.as_ref(),
        )
        .await
    }

    /// The checks a merge makes before it changes anything. A merge that
    /// pushes needs an upstream for the default branch.
    async fn validate_merge(&self, selector: &str, push: bool) -> Result<MergeSource> {
        let workspace = self.workspace(selector).await?;
        self.ensure_merging_enabled(&workspace).await?;
        let repo = self.repository(&workspace.repository_id).await?;
        ensure!(
            off_runtime::is_dir(&workspace.path).await?,
            "workspace directory is missing: {}",
            workspace.path.display()
        );
        self.verify_worktree(&workspace).await?;
        let default =
            crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Discover).await?;
        let branch = workspace.require_branch("merging")?.to_owned();
        ensure!(
            branch != default,
            "workspace {} is on the default branch {default}; nothing to merge",
            workspace.name
        );
        clean_branch(&workspace.path, &branch)
            .await
            .with_context(|| format!("workspace {} is not ready to merge", workspace.name))?;
        let source =
            git::resolve_commit(&workspace.path, &git::local_ref(&branch), git_run).await?;
        if let Some(checkout) = self.managed_checkout(&repo, &default).await? {
            bail!(
                "{default} is checked out in workspace {checkout}; merge needs it outside managed workspaces"
            );
        }
        let push = match push {
            true => Some(push_target(&repo, &default).await?),
            false => None,
        };
        Ok(MergeSource {
            workspace,
            branch,
            repo,
            default,
            source,
            push,
        })
    }

    /// Refuse to merge where `[merge] enabled` turns merging off, as in a
    /// repository that merges only through PRs.
    async fn ensure_merging_enabled(&self, workspace: &Workspace) -> Result<()> {
        ensure!(
            self.workspace_settings(workspace).await?.merge.enabled,
            "shoal merge is turned off for this repository ([merge] enabled = false); merge the workspace's PR with shoal pr merge"
        );
        Ok(())
    }

    /// Fast-forward the default branch from its upstream, or leave it as it
    /// is when it has none.
    async fn refresh_default(&self, repo: &Repository, default: &str) -> Result<PulledBranch> {
        if upstream(repo, &git::local_ref(default)).await?.is_none() {
            return Ok(PulledBranch::unchanged(
                repo,
                default,
                local_commit(repo, default).await?,
                format!("{default} has no upstream; merging on its local state"),
            ));
        }
        self.refresh_branch(repo, default, UpstreamPolicy::Required)
            .await
            .with_context(|| format!("could not refresh {default} before merging"))
    }

    /// What merging `workspace`'s `branch` at `head` into `default` at
    /// `default_commit` does: its commits, where it pushes, the sensitive
    /// files it brings to a default branch that changed them too, and the
    /// open PRs it bypasses.
    async fn merge_preview(
        &self,
        workspace: &Workspace,
        branch: &str,
        head: &str,
        default: &str,
        default_commit: &str,
        push: Option<&MergePush>,
    ) -> Result<MergePreview> {
        let id = workspace.id.clone();
        let open_prs = self
            .store
            .run(move |db| crate::daemon::links::wait::open_prs(db, &id))
            .await?;
        let warning = self.branch_merge_warning(&workspace.id).await?;
        Ok(MergePreview {
            workspace_id: workspace.id.clone(),
            branch: branch.to_owned(),
            head: head.to_owned(),
            default_branch: default.to_owned(),
            default_commit: default_commit.to_owned(),
            commits: git::status::commits_since(&workspace.path, default_commit).await?,
            push_to: push.cloned(),
            warning,
            open_prs,
        })
    }

    /// The managed workspace that has `branch` checked out, if any.
    async fn managed_checkout(&self, repo: &Repository, branch: &str) -> Result<Option<String>> {
        let Some(checkout) = git::checkout_of(&repo.path, branch, git::run).await? else {
            return Ok(None);
        };
        let Ok(path) = std::fs::canonicalize(&checkout.path) else {
            return Ok(None);
        };
        self.managed_workspace_at(&path).await
    }
}

/// Refuse a merge that is no longer the one the user confirmed: the branch
/// moved, the default branch took commits its upstream lacks, so the push
/// would publish them too, or the default branch's upstream changed.
async fn ensure_confirmed(merge: &MergeSource, confirmed: &MergeConfirmation) -> Result<()> {
    let (branch, default) = (&merge.branch, &merge.default);
    ensure!(
        merge.source == confirmed.head,
        "{branch} moved since you confirmed the merge; run shoal merge again"
    );
    ensure!(
        merge.push == confirmed.push,
        "where the merge pushes {default} changed since you confirmed it; run shoal merge again"
    );
    let current = local_commit(&merge.repo, default).await?;
    ensure!(
        current == confirmed.default_commit || synced_to(merge, confirmed, &current).await?,
        "{default} moved since you confirmed the merge; run shoal merge again"
    );
    Ok(())
}

/// Whether the default branch moved from the confirmed commit to `current`
/// only along its upstream, as a sync does, so the push publishes nothing
/// the user did not confirm.
async fn synced_to(
    merge: &MergeSource,
    confirmed: &MergeConfirmation,
    current: &str,
) -> Result<bool> {
    let Some(push) = &merge.push else {
        return Ok(false);
    };
    let path = &merge.repo.path;
    let tracking = format!("refs/remotes/{}", push.remote_branch());
    Ok(git::is_ancestor(
        path,
        &confirmed.default_commit,
        current,
        git::isolated_command,
    )
    .await?
        && git::ref_exists(path, &tracking, git::isolated_command).await?
        && git::is_ancestor(path, current, &tracking, git::isolated_command).await?)
}

/// The commit of `repo`'s local `branch`.
async fn local_commit(repo: &Repository, branch: &str) -> Result<String> {
    Ok(
        git::verify_revision(&repo.path, &git::local_ref(branch), git_run)
            .await
            .with_context(|| format!("repository has no local {branch} branch"))?
            .trim()
            .to_owned(),
    )
}

/// The upstream a merge pushes `default` to, refused when it has none.
async fn push_target(repo: &Repository, default: &str) -> Result<MergePush> {
    let (remote, reference) = upstream(repo, &git::local_ref(default))
        .await?
        .with_context(|| format!("{default} has no upstream to push to"))?;
    Ok(MergePush { remote, reference })
}
