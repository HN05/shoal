//! What becomes of an absorbed space's code: its branch merged into the
//! branch of the workspace that takes it, its uncommitted files carried
//! over there as uncommitted changes, under the repository's Git gate.
use shoal_links::pr::current_head;
use std::path::PathBuf;

use anyhow::Result;
use shoal_refs::{BranchName, CommitId};

use super::parties::{Code, Parties};
use crate::{
    daemon::{manager::Manager, workspace::squash_message},
    forge::items::action::MergeMethod,
    git::{
        fold::{self, Fold},
        merge_into::{Source, Target},
    },
    model::Workspace,
    state::WorkspaceState,
};
use shoal_protocol::absorb::{Absorb, AbsorbedCode};

/// What folding the target's code did.
#[derive(Default)]
pub(super) struct Folded {
    pub(super) merge: Option<AbsorbedCode>,
    pub(super) carried: Vec<String>,
    /// The target's commit that merged, past which the workspaces built on
    /// its branch rebase.
    pub(super) source: Option<CommitId>,
}

/// Both sides of a merge, read when it is about to happen.
struct Sides {
    repo: PathBuf,
    branch: BranchName,
    head: CommitId,
    method: MergeMethod,
    message: Option<String>,
    into: BranchName,
    into_head: CommitId,
    into_path: PathBuf,
    /// The target's worktree, whose uncommitted files carry over.
    carry_from: Option<PathBuf>,
}

impl Sides {
    fn fold(&self) -> Fold<'_> {
        Fold {
            repo: &self.repo,
            source: Some(Source {
                branch: &self.branch,
                commit: &self.head,
                method: self.method,
                message: self.message.as_deref(),
            }),
            carry_from: self.carry_from.as_deref(),
            into: Target {
                branch: &self.into,
                commit: &self.into_head,
                worktree: Some(&self.into_path),
            },
        }
    }
}

impl Manager {
    /// Refuse now what folding the target's code as `request` asks would
    /// refuse, before a forced absorb stops its agent.
    pub(super) async fn check_code(&self, parties: &Parties, request: &Absorb) -> Result<()> {
        let Code::Merge { target, into } = &parties.code else {
            return Ok(());
        };
        let (_repo, _git) = self.lock_repository(target.repository()).await?;
        let sides = self.sides(target, into, request).await?;
        fold::check(sides.fold()).await
    }

    /// Fold the target's code, its lifecycle reserved by the caller, into
    /// the space that takes it, as `request` asks, with that workspace
    /// reserved against every other lifecycle operation meanwhile.
    pub(super) async fn fold_code(&self, parties: &Parties, request: &Absorb) -> Result<Folded> {
        let Code::Merge { target, into } = &parties.code else {
            return Ok(Folded::default());
        };
        let (_repo, _git) = self.lock_repository(target.repository()).await?;
        let state = self.reserve_lifecycle(&into.id, WorkspaceState::Updating);
        let state = state.await?;
        let folded = self.fold_reserved(target, into, request).await;
        let restored = self.set_state(&into.id, state, into.error.clone());
        restored.await?;
        folded
    }

    /// Fold `target`'s code into `into`, both reserved. A squash or rebase
    /// records the target's completion as integrated, as `shoal done
    /// --integrated` does.
    async fn fold_reserved(
        &self,
        target: &Workspace,
        into: &Workspace,
        request: &Absorb,
    ) -> Result<Folded> {
        let sides = self.sides(target, into, request).await?;
        let folded = fold::fold(sides.fold()).await?;
        let merged = folded.merged.as_ref();
        let updated = merged.is_some_and(|merged| merged.updated);
        if updated && sides.method != MergeMethod::Merge {
            (self.record_integrated(&target.id, sides.head.clone())).await?;
        }
        Ok(Folded {
            merge: merged.map(|merged| AbsorbedCode {
                branch: sides.branch.clone(),
                into: sides.into.clone(),
                method: sides.method,
                commit: merged.commit.clone(),
                commits: folded.commits.clone(),
                updated: merged.updated,
            }),
            carried: folded.carried,
            source: Some(sides.head),
        })
    }

    /// The sides of merging `target`'s branch into `into`'s as `request`
    /// asks: by its method, else `[merge] method`, with its squash message,
    /// else one naming the absorbed space.
    async fn sides(&self, target: &Workspace, into: &Workspace, request: &Absorb) -> Result<Sides> {
        let settings = self.workspace_settings(into).await?;
        let method = (request.method.or(settings.merge.method)).unwrap_or(MergeMethod::Merge);
        let branch = target.require_branch("absorbing its branch")?.clone();
        let message = squash_message(method, request.message.as_deref())?;
        let message = message.or_else(|| {
            (method != MergeMethod::Rebase)
                .then(|| format!("Absorb {} from branch '{branch}'", target.name))
        });
        Ok(Sides {
            repo: self.repository(target.repository()).await?.path,
            head: current_head(target).await?,
            method,
            message,
            into: into.require_branch("taking another branch")?.clone(),
            into_head: current_head(into).await?,
            into_path: into.path.clone(),
            carry_from: (!request.discard_uncommitted).then(|| target.path.clone()),
            branch,
        })
    }
}
