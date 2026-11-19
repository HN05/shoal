//! Move stacked workspaces down when their base workspace's work merges:
//! through its PRs, or into the space that absorbs it.
use crate::forge::items::reference::ItemRef;
use crate::model::EventCause;
use anyhow::Result;
use rusqlite::params;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::{BranchName, CommitId};

use super::{
    pr::{RegistrationKind, current_head},
    wait,
};
use crate::{
    daemon::{agent_messages, manager::Manager, workspace::record_base_changed},
    forge::{
        PullRequest,
        items::link::ItemKind,
        updates::{Update, UpdateKind},
    },
    model::{Workspace, WorkspaceRef},
};
use shoal_forge::{
    RepoRequests, account::Account, items::retarget::RetargetRequests, updates::Edited,
};

impl Manager {
    /// Once every watched PR of `base` has merged, retarget the PRs of the
    /// workspaces stacked on it that target the head branch of one of them
    /// to the branch they merged into, stack them on
    /// its own base, and queue a `base_merged` update for their next watch.
    /// A failed merge lookup returns its error as `Ok(Some(_))`: the base's
    /// cleanup waits for the next sweep without reporting a failure.
    pub(super) async fn advance_stack(
        &self,
        base: &Workspace,
        registration: &RegistrationKind,
    ) -> Result<Option<anyhow::Error>> {
        let RegistrationKind::Watch { urls, .. } = registration else {
            return Ok(None);
        };
        if base.stacked_workspaces.is_empty() {
            return Ok(None);
        }
        // Its HEAD and remote authorize forge writes for other workspaces.
        self.verify_checked(base).await?;
        let mut merged = Vec::new();
        for url in urls {
            match self.base_pr_commits(base, url).await {
                Ok(Some(commits)) => merged.extend(commits),
                Ok(None) => return Ok(None),
                Err(error) => {
                    return Ok(Some(
                        error.context(format!("could not check whether {url} merged")),
                    ));
                }
            }
        }
        // The rebase cutoff is HEAD, so commits the merge left out would be
        // dropped from every stacked branch.
        let head = self
            .file_access
            .checked(&self.notifications, &base.path, current_head(base))
            .await?;
        if !merged.contains(&head) {
            return Ok(Some(anyhow::anyhow!(
                "the merged PRs do not contain the base workspace's HEAD"
            )));
        }
        let pulls = self.base_pulls(base, urls).await?;
        let Some(target) = merged_into(&pulls) else {
            return Ok(None);
        };
        // A watched PR's head need not be the base workspace's branch.
        let merged_heads: Vec<BranchName> = pulls.into_iter().map(|pull| pull.head).collect();
        // Reload: they may have moved since. The caller holds the PR gates of
        // those stacked when it read `base`, so one stacked since waits.
        let stacked = self
            .workspace_by_id(&base.id)
            .await?
            .part
            .stacked_workspaces;
        let held = |id: &SpaceId| base.stacked_workspaces.iter().any(|held| held.id == *id);
        if !stacked.iter().all(|stacked| held(&stacked.id)) {
            return Ok(Some(anyhow::anyhow!(
                "a workspace was stacked on it during the sweep"
            )));
        }
        let restack = Restack {
            base,
            head: &head,
            merged_heads: &merged_heads,
            target: &target,
            onto: Onto::Remote,
            new_base: base.base_workspace.as_ref().map(|base| base.id.clone()),
            cause: EventCause::Pr,
            account: &self.user_account(),
        };
        for stacked in stacked {
            self.restack(&stacked, &restack).await?;
        }
        Ok(None)
    }

    async fn base_pr_commits(
        &self,
        base: &Workspace,
        url: &ItemRef,
    ) -> Result<Option<Vec<CommitId>>> {
        let (forge, pull) = self.pr_forge(&base.path, &url.clone().into()).await?;
        forge
            .merged_commits(&self.user_account(), pull.pull()?)
            .await
    }

    /// The watched PRs `urls` of `base`, in order.
    async fn base_pulls(&self, base: &Workspace, urls: &[ItemRef]) -> Result<Vec<PullRequest>> {
        let mut pulls = Vec::new();
        for url in urls {
            let (forge, pull) = self.pr_forge(&base.path, &url.clone().into()).await?;
            pulls.push(forge.pull_request(&self.user_account(), &pull).await?);
        }
        Ok(pulls)
    }

    /// Stack `stacked` past the base `restack` names: retarget its PRs that
    /// still target a merged head, record its new base and queue a
    /// `base_merged` update for its next watch.
    pub(crate) async fn restack(
        &self,
        stacked: &WorkspaceRef,
        restack: &Restack<'_>,
    ) -> Result<()> {
        let Restack {
            base,
            head,
            merged_heads,
            target,
            ..
        } = *restack;
        let workspace = self.workspace_by_id(&stacked.id).await?;
        self.verify_checked(&workspace).await?;
        let id = workspace.id.clone();
        let items = self
            .store
            .run(move |db| wait::linked_items(db, &id, None))
            .await?;
        let mut updates = Vec::new();
        let mut retargeted = Vec::new();
        for url in items {
            let outcome = match url.kind().item() {
                Some(ItemKind::Pr) => {
                    let pr = StackedPr {
                        workspace: &workspace,
                        url: &url,
                        merged_heads,
                    };
                    self.retarget_stacked(pr, restack).await?
                }
                _ => None,
            };
            if let Some(Retarget::Done { from }) = &outcome {
                retargeted.push((url.clone(), Edited::retarget(from, target)));
            }
            updates.push(base_merged_update(&url, restack, outcome));
        }
        self.ensure_base_unchanged(base, head).await?;
        // Without a linked item to deliver an update with, its agent hears
        // of the rebase as a message, queued with the base change.
        let message = updates.is_empty().then(|| base_merged_message(restack, ""));
        let told = message.is_some();
        let stacked = Stacked {
            id: workspace.id,
            base: base.id.clone(),
            new_base: restack.new_base.clone(),
            cause: restack.cause,
        };
        let tell = Tell {
            updates,
            message,
            retargeted,
        };
        self.store
            .write(move |tx| record_restack(tx, &stacked, tell))
            .await?;
        if told {
            self.signals.message_queued();
        }
        Ok(())
    }

    /// Retarget a stacked PR that still targets the head branch of a merged
    /// PR of the base; `None` when it targets another. Changed ownership or
    /// base HEAD aborts the restack instead.
    async fn retarget_stacked(
        &self,
        pr: StackedPr<'_>,
        restack: &Restack<'_>,
    ) -> Result<Option<Retarget>> {
        let Restack {
            base,
            head,
            target,
            account,
            ..
        } = *restack;
        let StackedPr {
            workspace,
            url,
            merged_heads,
        } = pr;
        let lookup = async {
            let (forge, pull) = self.pr_forge(&workspace.path, &url.clone().into()).await?;
            let number = pull.pull()?;
            let current = forge.pull_request(account, &pull).await?.base;
            Ok::<_, anyhow::Error>(
                merged_heads
                    .contains(&current)
                    .then_some((forge, number, current)),
            )
        }
        .await;
        let (forge, number, from) = match lookup {
            Ok(Some(pr)) => pr,
            Ok(None) => return Ok(None),
            Err(error) => return Ok(Some(Retarget::Failed(error))),
        };
        // Lookups take time; recheck what authorizes the write right before it.
        self.verify_checked(workspace).await?;
        self.ensure_base_unchanged(base, head).await?;
        Ok(Some(match forge.retarget(account, number, target).await {
            Ok(()) => Retarget::Done { from },
            Err(error) => Retarget::Failed(error),
        }))
    }

    /// [`Manager::verify_worktree`] of `workspace`, inspected under
    /// `FileAccess::checked` since it may be in another directory than
    /// the workspace the sweep is handling.
    async fn verify_checked(&self, workspace: &Workspace) -> Result<()> {
        self.file_access
            .checked(
                &self.notifications,
                &workspace.path,
                self.verify_worktree(workspace),
            )
            .await
    }

    /// The rebase cutoff in the update is the base HEAD read before restacking.
    async fn ensure_base_unchanged(&self, base: &Workspace, head: &CommitId) -> Result<()> {
        let current = async {
            self.verify_worktree(base).await?;
            current_head(base).await
        };
        anyhow::ensure!(
            self.file_access
                .checked(&self.notifications, &base.path, current)
                .await?
                == *head,
            "the base workspace's HEAD changed while restacking; retrying next sweep"
        );
        Ok(())
    }
}

/// The branch the base's merged PRs `pulls` merged into: the base of the
/// last one that targets no other's head branch, since PRs stacked among
/// them merged into each other.
fn merged_into(pulls: &[PullRequest]) -> Option<BranchName> {
    let outside = |pull: &&PullRequest| !pulls.iter().any(|other| other.head == pull.base);
    let last = pulls.iter().rev().find(outside).or(pulls.last());
    last.map(|pull| pull.base.clone())
}

/// Where the workspaces stacked on a base go once its work merged.
pub(crate) struct Restack<'a> {
    pub(crate) base: &'a Workspace,
    /// The base's HEAD, past which stacked branches rebase.
    pub(crate) head: &'a CommitId,
    /// The branches a stacked PR may still target, which it retargets.
    pub(crate) merged_heads: &'a [BranchName],
    /// The branch the base's work merged into, which stacked ones rebase
    /// onto and their PRs target.
    pub(crate) target: &'a BranchName,
    pub(crate) onto: Onto,
    /// The workspace stacked ones build on next; none for the default
    /// branch.
    pub(crate) new_base: Option<SpaceId>,
    pub(crate) cause: EventCause,
    /// The account retargeting goes out as: the user's for Shoal's own
    /// sweeps and the user's requests, the agent account for an agent's.
    pub(crate) account: &'a Account,
}

/// Where the branch the base merged into is for a stacked branch to rebase
/// onto.
#[derive(Clone, Copy)]
pub(crate) enum Onto {
    /// On the remote, fetched first.
    Remote,
    /// A local branch of the repository.
    Local,
}

/// What a restack tells a stacked workspace's agent: `updates` for its
/// next watch, or else `message`, and the PRs Shoal `retargeted`, which it
/// takes as changed already.
struct Tell {
    updates: Vec<Update>,
    message: Option<String>,
    retargeted: Vec<(ItemRef, Edited)>,
}

/// A stacked workspace's new base, in place of `base`, recorded for `cause`.
struct Stacked {
    id: SpaceId,
    base: SpaceId,
    new_base: Option<SpaceId>,
    cause: EventCause,
}

/// A linked PR of the stacked `workspace`, and the head branches of the
/// base's merged PRs it may still target.
struct StackedPr<'a> {
    workspace: &'a Workspace,
    url: &'a ItemRef,
    merged_heads: &'a [BranchName],
}

/// How retargeting a stacked PR to the branch its base merged into went.
enum Retarget {
    /// Retargeted from the branch `from`.
    Done {
        from: BranchName,
    },
    Failed(anyhow::Error),
}

/// The `base_merged` update telling a stacked workspace's agent to rebase
/// past the base's HEAD onto the branch it merged into, with the outcome of
/// retargeting its PR.
fn base_merged_update(
    url: &ItemRef,
    restack: &Restack<'_>,
    retargeted: Option<Retarget>,
) -> Update {
    let target = restack.target;
    let retargeted = match retargeted {
        None => String::new(),
        Some(Retarget::Done { .. }) => format!("; Shoal retargeted this PR to {target}"),
        Some(Retarget::Failed(error)) => {
            format!("; retargeting this PR to {target} failed: {error:#}")
        }
    };
    Update {
        url: url.to_string(),
        kind: UpdateKind::BaseMerged,
        message: base_merged_message(restack, &retargeted),
        delivery: uuid::Uuid::new_v4().to_string(),
        check: None,
        workspace: None,
    }
}

/// What a `base_merged` update tells a stacked workspace's agent: where
/// its base merged, `retargeted`, and how to rebase onto it.
fn base_merged_message(restack: &Restack<'_>, retargeted: &str) -> String {
    let (target, head) = (restack.target, restack.head);
    let rebase = match restack.onto {
        Onto::Remote => format!("git fetch origin && git rebase --onto origin/{target} {head}"),
        Onto::Local => format!("git rebase --onto {target} {head}"),
    };
    format!(
        "Base workspace {} merged into {target}{retargeted}. Rebase onto it: {rebase}",
        restack.base.name,
    )
}

/// Atomically record the new base of the workspace `stacked` names,
/// journal the change, and tell its agent as `tell` says: queue its
/// updates for its next watch, or its message, and take the PRs Shoal
/// retargeted as changed already, since the updates say so. Does
/// nothing once it no longer builds on its old base: another sweep or a
/// manual change already moved it.
fn record_restack(tx: &rusqlite::Transaction<'_>, stacked: &Stacked, tell: Tell) -> Result<()> {
    let Tell {
        updates,
        message,
        retargeted,
    } = tell;
    let id = &stacked.id;
    let changed = tx.execute(
        "UPDATE workspaces SET base_workspace_id=?3 WHERE id=?1 AND base_workspace_id=?2",
        params![id, stacked.base, stacked.new_base],
    )?;
    if changed == 0 {
        return Ok(());
    }
    record_base_changed(tx, id, stacked.cause)?;
    for update in updates {
        wait::queue_update(tx, id, update)?;
    }
    if let Some(message) = message {
        agent_messages::queue_shoal_message(tx, id, &message)?;
    }
    for (url, edited) in retargeted {
        let change = wait::OwnChange {
            url: url.url(),
            authored: Vec::new(),
            state: None,
            edited,
        };
        wait::record_own_change(tx, id, change)?;
    }
    Ok(())
}
