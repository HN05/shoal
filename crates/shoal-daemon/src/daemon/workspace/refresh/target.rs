//! What a workspace was made from, and the commit that has now.
use anyhow::{Context, Result, bail, ensure};
use shoal_refs::CommitId;

use super::super::{Manager, copies::Source};
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::scope::Caller,
    forge::{PullRequest, items::reference::ItemInput, numbers::PullNumber},
    git::{self, default_branch::DefaultBranchLookup},
    model::{CopiedFrom, MadeFor, Repository, Workspace},
    protocol::{ItemContext, PullHead},
};

/// What a workspace was made from, which a refresh brings it up to date
/// with.
pub(super) enum Target {
    /// Its branch's upstream, the branch `reference` on `remote`.
    Upstream { remote: String, reference: String },
    /// The revision its branch builds on, resolved as `--base` resolves it:
    /// its base workspace's branch, or the branch its recorded base names.
    Base(String),
    /// The local default branch, synced first.
    DefaultBranch,
    /// The branch a workspace at a commit was made for, by the name given.
    BranchTip(String),
    /// The head of the PR a workspace at a commit was made for, as the
    /// forge reported it.
    PrHead(PullHead),
    /// The live workspace a copy copied, whose files it takes again.
    Copy(SpaceId),
    /// Something that never moves, such as a tag, by how a reader knows it.
    Fixed(String),
}

/// What a refresh found its target at.
pub(super) enum Found {
    /// What never moves, by how a reader knows it.
    Fixed(String),
    Moving(Move),
    /// The live workspace a copy copied, as a copy starts from it, and the
    /// command that makes a fresh copy of it.
    Copy {
        source: Box<Source>,
        fresh: String,
    },
}

/// The commit a refresh moves a workspace to, how Git or Shoal names what
/// holds it, and how the workspace gets there.
pub(super) struct Move {
    pub(super) commit: CommitId,
    pub(super) label: String,
    pub(super) how: How,
}

pub(super) enum How {
    /// Fast-forward its branch.
    FastForward,
    /// Check out the commit, without a branch, as where it starts.
    Restart,
}

impl Manager {
    /// What `workspace` was made from, with where a PR's head is read from
    /// the forge as `caller`.
    pub(super) async fn refresh_target(
        &self,
        workspace: &Workspace,
        caller: Option<&Caller>,
    ) -> Result<Target> {
        if let Some(copied) = &workspace.copied_from {
            return self.copy_target(workspace, copied, caller).await;
        }
        let repo = self.repository(&workspace.repository_id).await?;
        match (&workspace.branch, &workspace.at) {
            (Some(branch), _) => branch_target(&repo, workspace, branch).await,
            (None, Some(at)) => match &at.made_for {
                Some(MadeFor::Pr(number)) => self.pr_head(workspace, *number, caller).await,
                Some(MadeFor::Ref(name)) => made_for_ref(&repo, name).await,
                None => Ok(Target::Fixed(format!(
                    "commit {}",
                    at.commit.abbreviated(7)
                ))),
            },
            (None, None) => bail!("workspace {} records no start", workspace.name),
        }
    }

    /// What the copy `workspace` refreshes to: the live workspace it
    /// `copied`, which `caller`'s scope must reach as it must to copy it,
    /// while a copy of a checkpoint never moves.
    async fn copy_target(
        &self,
        workspace: &Workspace,
        copied: &CopiedFrom,
        caller: Option<&Caller>,
    ) -> Result<Target> {
        match (copied.checkpoint, &copied.workspace) {
            (Some(number), Some(source)) => Ok(Target::Fixed(format!(
                "checkpoint {number} of {}",
                source.name
            ))),
            (Some(number), None) => Ok(Target::Fixed(format!("checkpoint {number}"))),
            (None, Some(source)) => {
                if let Some(caller) = caller {
                    ensure!(
                        self.reaches(caller, &source.id).await?,
                        "workspace processes copy only their own workspace and those below it, and {} is outside this one's scope; ask the user to refresh {}",
                        source.name,
                        workspace.name
                    );
                }
                Ok(Target::Copy(source.id.clone()))
            }
            (None, None) => match self.removed_source_name(&workspace.id).await? {
                Some(name) => bail!("source {name} was removed; nothing to refresh from"),
                None => bail!(
                    "the source of {} was removed; nothing to refresh from",
                    workspace.name
                ),
            },
        }
    }

    /// The head of PR `number` of the repository of `workspace`, read as
    /// `caller`.
    async fn pr_head(
        &self,
        workspace: &Workspace,
        number: PullNumber,
        caller: Option<&Caller>,
    ) -> Result<Target> {
        let context = ItemContext::Workspace(SpaceSelector::id(&workspace.id));
        let pull = ItemInput::Number(number.get());
        let pull = self.pull_request(&context, &pull, caller).await?;
        Ok(Target::PrHead(pull_head(pull)?))
    }

    /// Where `target` is now in `repo`, fetching what it names on a remote;
    /// the caller holds the repository's Git gate.
    pub(super) async fn find_target(&self, repo: &Repository, target: Target) -> Result<Found> {
        let (commit, label, how) = match target {
            Target::Fixed(what) => return Ok(Found::Fixed(what)),
            Target::Copy(id) => {
                let fresh = format!(
                    "shoal start --copy {}",
                    self.workspace_by_id(&id).await?.number
                );
                let source = Box::new(self.live_copy_source(&id).await?);
                return Ok(Found::Copy { source, fresh });
            }
            Target::Upstream { remote, reference } => {
                let commit = git::fetch::reference(&repo.path, &remote, &reference).await?;
                let branch = git::strip_local(&reference).unwrap_or(&reference);
                (commit, format!("{remote}/{branch}"), How::FastForward)
            }
            Target::Base(base) => {
                let revision = self.new_branch_base(repo, Some(&base)).await?;
                let commit = resolve(repo, &revision, short_ref(&base)).await?;
                (commit, short_ref(&base).to_owned(), How::FastForward)
            }
            Target::DefaultBranch => {
                let default =
                    git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
                let commit = resolve(repo, &git::local_ref(&default), default.as_str()).await?;
                (commit, default.to_string(), How::FastForward)
            }
            Target::BranchTip(name) => {
                let start = self.resolve_revision(repo, Some(&name)).await?;
                (start.at.commit, name, How::Restart)
            }
            Target::PrHead(head) => {
                let start = super::super::commits::pull_head(repo, &head).await?;
                let label = format!("PR #{}'s head", head.number);
                (start.at.commit, label, How::Restart)
            }
        };
        Ok(Found::Moving(Move { commit, label, how }))
    }
}

/// What the branch workspace on `branch` builds on: its upstream, else its
/// base workspace's branch or the branch its recorded base names, else the
/// default branch.
async fn branch_target(
    repo: &Repository,
    workspace: &Workspace,
    branch: &shoal_refs::BranchName,
) -> Result<Target> {
    if let Some((remote, reference)) = git::repo::upstream(repo, &git::local_ref(branch)).await? {
        return Ok(Target::Upstream { remote, reference });
    }
    if let Some(base) = (workspace.base_workspace.as_ref()).and_then(|base| base.branch.clone()) {
        return Ok(Target::Base(base.into()));
    }
    let default = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await;
    Ok(match (&workspace.base_ref, default) {
        (Some(base), Ok(default)) if *base != git::local_ref(&default) => {
            Target::Base(base.clone())
        }
        _ => Target::DefaultBranch,
    })
}

/// What a workspace at a commit made for `name` follows: a branch's tip,
/// while a tag never moves.
async fn made_for_ref(repo: &Repository, name: &str) -> Result<Target> {
    // A branch only a remote has, never fetched, resolves to nothing here.
    let reference = git::symbolic_revision(&repo.path, name, git::run_isolated).await;
    Ok(match reference {
        Ok(reference) if reference.starts_with("refs/tags/") => {
            Target::Fixed(format!("tag {name}"))
        }
        _ => Target::BranchTip(name.to_owned()),
    })
}

/// Where `pull`'s head is: the ref its forge serves it under, else its
/// branch here, at the commit the forge reported.
fn pull_head(pull: PullRequest) -> Result<PullHead> {
    let reference = match pull.head_ref {
        Some(reference) => reference,
        None if !pull.fork => git::local_ref(&pull.head),
        None => bail!(
            "PR #{} comes from a fork, and its forge serves no ref for a fork's head",
            pull.number
        ),
    };
    let commit = (pull.head_commit)
        .with_context(|| format!("PR #{} reports no head commit", pull.number))?;
    Ok(PullHead {
        number: pull.number,
        reference,
        commit,
        base: None,
    })
}

/// The commit `revision`, which `label` names, has in `repo`.
async fn resolve(repo: &Repository, revision: &str, label: &str) -> Result<CommitId> {
    git::resolve_commit(&repo.path, revision, git::run_isolated)
        .await
        .with_context(|| format!("{label} is not a commit here"))
}

/// A branch's name as Git shortens its full ref.
fn short_ref(reference: &str) -> &str {
    git::strip_local(reference)
        .or_else(|| git::strip_remote(reference))
        .unwrap_or(reference)
}
