//! Merge preparation: validate a workspace branch, say what merging it does,
//! and refresh the default branch before the merge worker merges it.
mod approval;
mod pushed;
mod records;
mod unmerge;

pub(crate) use records::RecordChange;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use shoal_refs::{BranchName, CommitId};

use super::super::{manager::Manager, workspace::branches::UpstreamPolicy};
use crate::{
    daemon::{reviews::gate::Gate, scope::Caller},
    forge::items::action::MergeMethod,
    fsutil::off_runtime,
    git::{
        self,
        default_branch::DefaultBranchLookup,
        existing_branch::tracking_ref,
        repo::{clean_branch, merge_temporaries, unfinished_operation, upstream},
        run_isolated as git_run,
    },
    model::{
        MergeConfirmation, MergeOptions, MergePath, MergePlan, MergePreview, MergePush,
        PulledBranch, Repository, Workspace,
    },
};
use shoal_protocol::space_names::SpaceSelector;

/// A workspace's branch at the commit that merges, by its method.
struct Merged<'a> {
    workspace: &'a Workspace,
    branch: &'a BranchName,
    head: &'a CommitId,
    method: MergeMethod,
    message: Option<&'a str>,
}

/// A workspace branch that passed every check a merge makes before it
/// changes anything.
struct MergeSource {
    workspace: Workspace,
    branch: BranchName,
    repo: Repository,
    default: BranchName,
    /// The branch's commit that merges.
    source: CommitId,
    method: MergeMethod,
    message: Option<String>,
    push: Option<MergePush>,
}

impl Manager {
    /// What merging the workspace `selector` names as `options` say does,
    /// after the checks `caller`'s merge makes before it changes anything.
    pub async fn preview_merge(
        &self,
        selector: &SpaceSelector,
        options: &MergeOptions,
        caller: Option<&Caller>,
    ) -> Result<MergePreview> {
        let merge = self.validate_merge(selector, options, caller).await;
        let merge = self.queue_unreviewed(merge).await?;
        let default_commit = local_commit(&merge.repo, &merge.default).await?;
        let merged = Merged {
            workspace: &merge.workspace,
            branch: &merge.branch,
            head: &merge.source,
            method: merge.method,
            message: merge.message.as_deref(),
        };
        let onto = (&merge.default, &default_commit);
        self.merge_preview(merged, onto, merge.push.as_ref()).await
    }

    /// Validate and refresh `caller`'s merge, which runs as `options` say
    /// while it holds the repository Git gate. The gate must stay held until
    /// the tracked execution completes; a review gate's [`Unreviewed`]
    /// refusal is left for the caller to queue once it releases the gate.
    ///
    /// [`Unreviewed`]: crate::daemon::reviews::gate::Unreviewed
    pub async fn prepare_merge(
        &self,
        selector: &SpaceSelector,
        options: &MergeOptions,
        caller: Option<&Caller>,
    ) -> Result<MergePlan> {
        let merge = self.validate_merge(selector, options, caller).await?;
        if let Some(confirmed) = &options.confirmed {
            let warning = self
                .branch_merge_warning(&SpaceSelector::id(&merge.workspace.id))
                .await?;
            let warned = warning.map(|warning| warning.files).unwrap_or_default();
            ensure_confirmed(&merge, confirmed, &warned).await?;
        }
        let MergeSource {
            workspace,
            branch,
            repo,
            default,
            source,
            method,
            message,
            push,
        } = merge;
        let default_refresh = self.refresh_default(&repo, &default).await?;
        let checkout = clean_checkout(&repo, &default).await?;
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
            method,
            message,
            checkout,
            merge_temporaries,
            default_refresh,
            push,
        })
    }

    /// What merging `plan` does, for the approval of a scoped merge.
    async fn plan_preview(&self, plan: &MergePlan) -> Result<MergePreview> {
        let merged = Merged {
            workspace: &plan.workspace,
            branch: &plan.branch,
            head: &plan.source,
            method: plan.method,
            message: plan.message.as_deref(),
        };
        let onto = (&plan.default_refresh.branch, &plan.default_refresh.commit);
        self.merge_preview(merged, onto, plan.push.as_ref()).await
    }

    /// The checks `caller`'s merge as `options` say makes before it changes
    /// anything, its review gate last, which queues no review. A merge that
    /// pushes needs an upstream for the default branch.
    async fn validate_merge(
        &self,
        selector: &SpaceSelector,
        options: &MergeOptions,
        caller: Option<&Caller>,
    ) -> Result<MergeSource> {
        let workspace = self.workspace(selector).await?;
        let (method, message) = self.merge_method(&workspace, options).await?;
        self.ensure_merges_locally(&workspace).await?;
        let repo = self.repository(workspace.repository()).await?;
        ensure!(
            off_runtime::is_dir(&workspace.path).await?,
            "workspace directory is missing: {}",
            workspace.path.display()
        );
        self.verify_worktree(&workspace).await?;
        let default =
            crate::git::default_branch::resolve(&repo.path, DefaultBranchLookup::Discover).await?;
        let branch = workspace.require_branch("merging")?.clone();
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
        self.ensure_unmanaged_checkout(&repo, &default, "merge")
            .await?;
        let push = match options.push {
            true => Some(push_target(&repo, &default).await?),
            false => None,
        };
        self.check_review(&workspace, Gate::Merge, &source, caller)
            .await?;
        Ok(MergeSource {
            workspace,
            branch,
            repo,
            default,
            source,
            method,
            message,
            push,
        })
    }

    /// The method and squash message `workspace` merges by as `options`
    /// ask, else by its `[merge] method`, else by a merge commit or
    /// fast-forward; refused where `[merge] enabled` turns local merges
    /// off, as in a repository that merges only through PRs.
    async fn merge_method(
        &self,
        workspace: &Workspace,
        options: &MergeOptions,
    ) -> Result<(MergeMethod, Option<String>)> {
        let settings = self.workspace_settings(workspace).await?;
        let settings = &settings.merge;
        ensure!(
            settings.enabled,
            "local merges are turned off for this repository ([merge] enabled = false); open a PR with shoal pr open, then merge it with shoal merge"
        );
        let method = (options.method.or(settings.method)).unwrap_or(MergeMethod::Merge);
        Ok((method, squash_message(method, options.message.as_deref())?))
    }

    /// Refuse a local merge of a workspace whose work merges through its
    /// open PR, which the merge would bypass, however the request came.
    async fn ensure_merges_locally(&self, workspace: &Workspace) -> Result<()> {
        let path = self.merge_path(&SpaceSelector::id(&workspace.id)).await?;
        if let MergePath::Pr { pr } = path {
            bail!(
                "{pr} is open, so shoal merge merges it on the forge; close it to merge workspace {} locally",
                workspace.name
            );
        }
        Ok(())
    }

    /// Fast-forward the default branch from its upstream, or leave it as it
    /// is when it has none.
    async fn refresh_default(
        &self,
        repo: &Repository,
        default: &BranchName,
    ) -> Result<PulledBranch> {
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

    /// What merging `merged` into the default branch `onto`, at its commit,
    /// does: its commits, where it pushes, the sensitive files it brings to
    /// a default branch that changed them too.
    async fn merge_preview(
        &self,
        merged: Merged<'_>,
        onto: (&BranchName, &CommitId),
        push: Option<&MergePush>,
    ) -> Result<MergePreview> {
        let (workspace, (default, default_commit)) = (merged.workspace, onto);
        let warning = self
            .branch_merge_warning(&SpaceSelector::id(&workspace.id))
            .await?;
        Ok(MergePreview {
            workspace_id: workspace.id.clone(),
            branch: merged.branch.clone(),
            head: merged.head.clone(),
            method: merged.method,
            message: merged.message.map(str::to_owned),
            default_branch: default.clone(),
            default_commit: default_commit.clone(),
            commits: git::status::commits_since(&workspace.path, default_commit.as_str()).await?,
            push_to: push.cloned(),
            unpushed: match push {
                Some(_) => unpushed(&workspace.path, default, default_commit).await?,
                None => 0,
            },
            warning,
        })
    }

    /// Refuse `command` while a managed workspace has the default branch
    /// `default` checked out.
    async fn ensure_unmanaged_checkout(
        &self,
        repo: &Repository,
        default: &BranchName,
        command: &str,
    ) -> Result<()> {
        if let Some(checkout) = self.managed_checkout(repo, default).await? {
            bail!(
                "{default} is checked out in workspace {checkout}; {command} needs it outside managed workspaces"
            );
        }
        Ok(())
    }

    /// The managed workspace that has `branch` checked out, if any.
    async fn managed_checkout(
        &self,
        repo: &Repository,
        branch: &BranchName,
    ) -> Result<Option<String>> {
        let Some(checkout) = git::checkout_of(&repo.path, branch, git::run).await? else {
            return Ok(None);
        };
        let Ok(path) = std::fs::canonicalize(&checkout.path) else {
            return Ok(None);
        };
        self.managed_workspace_at(&path).await
    }
}

/// `message`, refused for a merge by `method` other than a squash, or when
/// it is empty.
fn squash_message(method: MergeMethod, message: Option<&str>) -> Result<Option<String>> {
    let Some(message) = message else {
        return Ok(None);
    };
    ensure!(
        method == MergeMethod::Squash,
        "a merge message is a squash merge's commit message; this merge is by {method}, so pass -X squash"
    );
    ensure!(!message.trim().is_empty(), "the merge message is empty");
    Ok(Some(message.to_owned()))
}

/// Refuse a merge that is no longer the one the user confirmed: the branch
/// moved, the default branch took commits its upstream lacks, so the push
/// would publish them too, the default branch's upstream changed, or a
/// conflict check found other sensitive files than the `warned` ones the
/// question named.
async fn ensure_confirmed(
    merge: &MergeSource,
    confirmed: &MergeConfirmation,
    warned: &[String],
) -> Result<()> {
    let (branch, default) = (&merge.branch, &merge.default);
    ensure!(
        confirmed.changed_on_both_sides == warned,
        "a conflict check found other files changed on both sides since you confirmed the merge; run shoal merge again"
    );
    ensure!(
        merge.source == confirmed.head,
        "{branch} moved since you confirmed the merge; run shoal merge again"
    );
    ensure!(
        (merge.method, &merge.message) == (confirmed.method, &confirmed.message),
        "the merge's method or message is not the one you confirmed; run shoal merge again"
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
    current: &CommitId,
) -> Result<bool> {
    let path = &merge.repo.path;
    if merge.push.is_none() {
        return Ok(false);
    }
    let Some(tracking) = tracking_ref(path, &merge.default).await? else {
        return Ok(false);
    };
    Ok(git::is_ancestor(
        path,
        confirmed.default_commit.as_str(),
        current.as_str(),
        git::isolated_command,
    )
    .await?
        && git::is_ancestor(path, current.as_str(), &tracking, git::isolated_command).await?)
}

/// The checkout of `repo`'s `default` branch, if it has one, which must be
/// clean and between Git operations before a merge or unmerge moves it.
async fn clean_checkout(repo: &Repository, default: &BranchName) -> Result<Option<PathBuf>> {
    let checkout = git::checkout_of(&repo.path, default, git::run)
        .await?
        .map(|tree| tree.path);
    if let Some(checkout) = &checkout {
        clean_branch(checkout, default).await?;
        if let Some(marker) = unfinished_operation(checkout).await? {
            bail!("{default} has an unfinished Git operation ({marker})");
        }
    }
    Ok(checkout)
}

/// The commits of `default_commit` that the upstream of `default`, which a
/// merge pushes to, lacks as the last fetch saw it; all of them when nothing
/// was fetched from it yet.
async fn unpushed(path: &Path, default: &BranchName, default_commit: &CommitId) -> Result<u64> {
    match tracking_ref(path, default).await? {
        Some(tracking) => {
            git::status::commits_between(path, &tracking, default_commit.as_str()).await
        }
        None => git::status::commits_of(path, default_commit.as_str()).await,
    }
}

/// The commit of `repo`'s local `branch`.
async fn local_commit(repo: &Repository, branch: &BranchName) -> Result<CommitId> {
    Ok(
        git::verify_revision(&repo.path, &git::local_ref(branch), git_run)
            .await
            .with_context(|| format!("repository has no local {branch} branch"))?
            .trim()
            .into(),
    )
}

/// The upstream a merge pushes `default` to, refused when it has none.
async fn push_target(repo: &Repository, default: &BranchName) -> Result<MergePush> {
    let (remote, reference) = upstream(repo, &git::local_ref(default))
        .await?
        .with_context(|| format!("{default} has no upstream to push to"))?;
    Ok(MergePush { remote, reference })
}
