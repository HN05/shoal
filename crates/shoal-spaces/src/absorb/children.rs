//! The spaces directly below an absorbed space, which move up one level.
//! Those built on its branch restack onto the branch of the workspace that
//! merged it, unless that branch builds on them, else onto what the
//! absorbed branch built on.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, ensure};
use shoal_forge::transport::account::Account;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::{BranchName, CommitId};

use super::{code::Folded, parties::Parties};
use crate::daemon::{Onto, Restack};
use crate::{
    git::default_branch::{self, DefaultBranchLookup},
    model::{EventCause, Space, SpaceRef, Workspace},
    {gates::PrGuard, scope::Caller},
};

impl Spaces {
    /// Restacks every workspace built on the target's branch onto where its
    /// code went, as `folded` says, with `caller`'s account, and returns the
    /// spaces that moved up. One that cannot move stops the absorb before
    /// the removal, which would take it.
    pub(super) async fn lift_children(
        &self,
        daemon: &dyn Daemon,
        parties: &Parties,
        folded: &Folded,
        caller: Option<&Caller>,
    ) -> Result<Vec<SpaceRef>> {
        let account = self.forges.read_account(caller);
        // Base changes take these gates, so the stack stays as planned.
        let _gates = self.lock_stack_gates(parties).await?;
        let children = self.children(&parties.target.id).await?;
        let stack = self.absorbed_stack(daemon, parties, folded).await?;
        let restack =
            |id: &SpaceId| (stack.as_ref()).map(|(base, ways)| ways.of(id).restack(base, &account));
        for child in &children {
            (self.move_up_one_level(daemon, &child.id, restack(&child.id).as_ref()))
                .await
                .with_context(|| {
                    format!(
                        "{} could not move up, so the absorbed space stays",
                        child.name
                    )
                })?;
        }
        // Those built on its branch elsewhere in the tree stay where they
        // are.
        let stacked = stack.iter().flat_map(|(base, _)| &base.stacked_workspaces);
        for other in stacked.filter(|stacked| !children.iter().any(|child| child.id == stacked.id))
        {
            if let Some(restack) = restack(&other.id) {
                daemon.restack(other, &restack).await?;
            }
        }
        Ok(children)
    }

    /// Holds the PR gates every change of a workspace's base takes: of the
    /// target, the space that takes its work, the spaces below the target
    /// and the workspaces built on its branch. Refused when another
    /// workspace came to build on its branch meanwhile.
    async fn lock_stack_gates(&self, parties: &Parties) -> Result<PrGuard> {
        let ids = self.stack_gate_ids(parties).await?;
        let gates = self.gates.lock_prs(&ids).await;
        let now = self.stack_gate_ids(parties).await?;
        ensure!(
            now.iter().all(|id| ids.contains(id)),
            "a workspace came to build on {} meanwhile; absorb it again",
            parties.target.name
        );
        Ok(gates)
    }

    async fn stack_gate_ids(&self, parties: &Parties) -> Result<Vec<SpaceId>> {
        let (target, into) = (&parties.target.id, &parties.into.id);
        let mut ids = vec![target.clone(), into.clone()];
        ids.extend(
            self.children(target)
                .await?
                .into_iter()
                .map(|child| child.id),
        );
        if parties.target.worktree().is_some() {
            let stacked = self.workspace_by_id(target).await?.part.stacked_workspaces;
            ids.extend(stacked.into_iter().map(|stacked| stacked.id));
        }
        Ok(ids)
    }

    /// None unless workspaces build on the absorbed workspace's branch.
    async fn absorbed_stack(
        &self,
        daemon: &dyn Daemon,
        parties: &Parties,
        folded: &Folded,
    ) -> Result<Option<(Workspace, Ways)>> {
        let Some(target) = parties.target.to_workspace() else {
            return Ok(None);
        };
        let base = self.workspace_by_id(&target.id).await?;
        if base.stacked_workspaces.is_empty() {
            return Ok(None);
        }
        let head = daemon.current_head(&base).await?;
        // A workspace built on it rebases past exactly what merged.
        if let Some(source) = &folded.source {
            ensure!(
                head == *source,
                "{} moved after its branch merged; absorb it again",
                base.name
            );
        }
        let merged = (folded.merge.as_ref()).map(|merge| RestackParts {
            head: head.clone(),
            merged_heads: base.branch().cloned().into_iter().collect(),
            target: merge.into.clone(),
            onto: Onto::Local,
            new_base: Some(parties.into.id.clone()),
        });
        let aside = self.restack_aside(daemon, &base, head).await?;
        let ways = Ways {
            merged,
            held: RestackParts {
                onto: Onto::Held,
                ..aside.clone()
            },
            aside,
            under_taker: self.taker_and_bases(&parties.into).await?,
        };
        Ok(Some((base, ways)))
    }

    /// Used when the work did not merge where they can follow it: onto the
    /// branch the absorbed workspace builds on, as when its PRs merge, else
    /// onto the default branch.
    async fn restack_aside(
        &self,
        daemon: &dyn Daemon,
        base: &Workspace,
        head: CommitId,
    ) -> Result<RestackParts> {
        let merged_heads = base.branch().cloned().into_iter().collect();
        let own_base = (base.base_workspace.as_ref())
            .and_then(|own| Some((own.id.clone(), own.branch.clone()?)));
        Ok(match own_base {
            Some((id, branch)) => RestackParts {
                head,
                merged_heads,
                target: branch,
                onto: Onto::Local,
                new_base: Some(id),
            },
            None => {
                let repo = daemon
                    .repository(crate::repository::Selector::from(base.repository()))
                    .await?;
                let default = default_branch::resolve(&repo.path, DefaultBranchLookup::Cached);
                RestackParts {
                    head,
                    merged_heads,
                    target: default.await?,
                    onto: Onto::Remote,
                    new_base: None,
                }
            }
        })
    }

    /// Nearest first.
    async fn taker_and_bases(&self, space: &Space) -> Result<Vec<SpaceId>> {
        let mut bases = vec![space.id.clone()];
        let mut next = space
            .to_workspace()
            .and_then(|workspace| workspace.base_workspace.clone());
        while let Some(base) = next.filter(|base| !bases.contains(&base.id)) {
            next = match self.find_workspace(&base.id).await? {
                Some(workspace) => workspace.part.base_workspace,
                None => None,
            };
            bases.push(base.id);
        }
        Ok(bases)
    }
}

struct Ways {
    /// Onto the branch that merged its work.
    merged: Option<RestackParts>,
    /// Onto what it built on itself.
    aside: RestackParts,
    /// As `aside`, for the workspace that took the work and those its branch
    /// builds on: that branch holds the work, so neither it nor one below it
    /// drops the work by a rebase.
    held: RestackParts,
    /// The workspace that merged the work and those its branch builds on,
    /// which cannot build on that branch in turn.
    under_taker: Vec<SpaceId>,
}

impl Ways {
    fn of(&self, id: &SpaceId) -> &RestackParts {
        match &self.merged {
            Some(_) if self.under_taker.contains(id) => &self.held,
            Some(merged) => merged,
            None => &self.aside,
        }
    }
}

/// What [`Restack`] borrows, owned.
#[derive(Clone)]
struct RestackParts {
    head: CommitId,
    merged_heads: Vec<BranchName>,
    target: BranchName,
    onto: Onto,
    new_base: Option<SpaceId>,
}

impl RestackParts {
    fn restack<'a>(&'a self, base: &'a Workspace, account: &'a Account) -> Restack<'a> {
        Restack {
            base,
            head: &self.head,
            merged_heads: &self.merged_heads,
            target: &self.target,
            onto: self.onto,
            new_base: self.new_base.clone(),
            cause: EventCause::Absorbed,
            account,
        }
    }
}
