//! Who an absorb involves and what becomes of the absorbed space's code,
//! each refused before anything changes.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, ensure};
use shoal_ids::space_ids::SpaceId;

use super::{code::Folded, records::TakenOver};
use crate::{
    git::{self, default_branch::DefaultBranchLookup},
    model::{Space, SpaceRef, Workspace},
    protocol::QueuedLaunches,
    removal::UnsavedWork,
    scope::Caller,
    state::WorkspaceState,
};
use shoal_protocol::{
    absorb::{Absorb, Absorbed},
    space_names::SpaceSelector,
};

pub(super) struct Parties {
    pub(super) target: Space,
    pub(super) into: Space,
    pub(super) code: Code,
    /// The target, or a chat in its worktree, is not done, and the request
    /// forces the absorb.
    pub(super) forced: bool,
    /// The chats in the target's worktree, which go with it.
    pub(super) chats: Vec<SpaceRef>,
    /// Every space the target's removal takes with it once the spaces below
    /// it moved up: those chats and what their removal takes.
    pub(super) going: Vec<SpaceId>,
    /// The user holds the target and the caller may release the hold.
    pub(super) release_holds: bool,
}

pub(super) enum Code {
    None,
    Merge {
        target: Box<Workspace>,
        into: Box<Workspace>,
    },
    /// It has nowhere to go and is left behind, which its removal keeps.
    Discard,
}

impl Parties {
    pub(super) fn absorbed(
        &self,
        folded: Folded,
        children: Vec<SpaceRef>,
        taken: TakenOver,
    ) -> Absorbed {
        Absorbed {
            space: self.target.space_ref(),
            into: self.into.space_ref(),
            merge: folded.merge,
            carried: folded.carried,
            discarded: matches!(self.code, Code::Discard),
            links: taken.links,
            files: taken.files,
            threads: taken.threads,
            children,
            withdrawn: taken.withdrawn,
            warnings: taken.warnings,
        }
    }
}

impl Spaces {
    /// The caller's scope already confined the target to below its own space
    /// and the space that takes it to one it reaches.
    pub(super) async fn absorb_parties(
        &self,
        daemon: &dyn Daemon,
        request: &Absorb,
        caller: Option<&Caller>,
    ) -> Result<Parties> {
        let target = self.space(&request.space).await?;
        let into = self
            .absorber(&target, request.into.as_ref(), caller)
            .await?;
        let chats = self.chats_in(&target.id).await?;
        let going = self.going_with(&chats).await?;
        self.ensure_outside(&target, &into, &going).await?;
        let forced = self
            .ensure_done(daemon, &target, &chats, request.force)
            .await?;
        self.ensure_settled(&target, &into).await?;
        let code = self.code_of(daemon, &target, &into, request.force).await?;
        let release_holds = self.absorb_holds(daemon, &target, &going, caller).await?;
        Ok(Parties {
            target,
            into,
            code,
            forced,
            chats,
            going,
            release_holds,
        })
    }

    /// Read before the work moves: the target's files, which moved or its
    /// checkpoint keeps, the commits of a detached HEAD, which a forced
    /// absorb leaves behind, and the spaces that go with it.
    pub(super) async fn removal_consent(
        &self,
        daemon: &dyn Daemon,
        parties: &Parties,
    ) -> Result<UnsavedWork> {
        let detached_head = match parties.target.to_workspace() {
            Some(target) if target.branch().is_none() => Some(daemon.current_head(&target).await?),
            _ => None,
        };
        Ok(UnsavedWork::Confirmed {
            files: true,
            detached_head,
            files_restorable: true,
            children: Some(parties.going.clone()),
        })
    }

    /// No removal, stop or other lifecycle operation runs in the target
    /// meanwhile.
    pub(super) async fn while_target_reserved<T>(
        &self,
        parties: &Parties,
        work: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let Some(target) = parties.target.to_workspace() else {
            return work.await;
        };
        let state = (self.reserve_lifecycle(&target.id, WorkspaceState::Updating)).await?;
        let done = work.await;
        self.set_state(&target.id, state, target.error.clone())
            .await?;
        done
    }

    /// A forced absorb takes them as they are.
    pub(super) async fn stop_absorbed(&self, daemon: &dyn Daemon, parties: &Parties) -> Result<()> {
        let spaces =
            std::iter::once(&parties.target.id).chain(parties.chats.iter().map(|chat| &chat.id));
        for id in spaces {
            let selector = SpaceSelector::id(id);
            (daemon.stop_space_manually(&selector, false, QueuedLaunches::Cancel)).await?;
        }
        Ok(())
    }

    /// The one `into` names, else the caller's own, else, for the user, the
    /// target's parent.
    async fn absorber(
        &self,
        target: &Space,
        into: Option<&SpaceSelector>,
        caller: Option<&Caller>,
    ) -> Result<Space> {
        match (into, caller) {
            (Some(into), _) => self.space(into).await,
            (None, Some(caller)) => self.space(&SpaceSelector::id(&caller.space_id)).await,
            (None, None) => {
                let parent = (target.parent.as_ref()).with_context(|| {
                    format!(
                        "{} is a top-level space; name the space that takes its work with --into",
                        target.label()
                    )
                })?;
                self.space(&SpaceSelector::id(&parent.id)).await
            }
        }
    }

    /// Walks them as the shared removal does.
    async fn going_with(&self, chats: &[SpaceRef]) -> Result<Vec<SpaceId>> {
        let mut going = Vec::new();
        for chat in chats {
            going.push(chat.id.clone());
            let taken = self.spaces_taken_with(&chat.id).await?;
            going.extend(taken.into_iter().map(|space| space.id));
        }
        Ok(going)
    }

    /// Refuses absorbing the target into itself, a space below it, or one of
    /// the spaces `going` with it, which the absorb moves or removes.
    async fn ensure_outside(&self, target: &Space, into: &Space, going: &[SpaceId]) -> Result<()> {
        let inside = self.within_subtree(&target.id, &into.id).await? || going.contains(&into.id);
        ensure!(
            !inside,
            "{} is {} or goes with it, which absorbing it moves or removes; name another space with --into",
            into.label(),
            target.label()
        );
        Ok(())
    }

    /// Refuses while another lifecycle operation, such as a stop or a
    /// removal, runs in either workspace.
    async fn ensure_settled(&self, target: &Space, into: &Space) -> Result<()> {
        for space in [target, into] {
            if space.worktree().is_some() {
                self.check_lifecycle(&space.id).await?;
            }
        }
        Ok(())
    }

    /// Forced when the target, or one of the `chats` in its worktree, is not
    /// done, which only `force` absorbs.
    async fn ensure_done(
        &self,
        daemon: &dyn Daemon,
        target: &Space,
        chats: &[SpaceRef],
        force: bool,
    ) -> Result<bool> {
        let mut undone = Vec::new();
        if !self.done_at_head(daemon, target).await? {
            undone.push(target.label());
        }
        for chat in chats {
            if self.completion(&chat.id).await?.is_none() {
                undone.push(format!("chat {} in its worktree", chat.name));
            }
        }
        if undone.is_empty() {
            return Ok(false);
        }
        ensure!(
            force,
            "not done: {}; finish the work with shoal done first, or pass --force to stop its agents and absorb it as it is",
            undone.join(", ")
        );
        Ok(true)
    }

    /// For a workspace, the completion must be at its current HEAD.
    async fn done_at_head(&self, daemon: &dyn Daemon, space: &Space) -> Result<bool> {
        let Some(completion) = self.completion(&space.id).await? else {
            return Ok(false);
        };
        Ok(match space.to_workspace() {
            Some(workspace) => daemon.current_head(&workspace).await? == completion.head,
            None => true,
        })
    }

    /// A workspace's branch merges into that of a workspace of its
    /// repository. Code with nowhere else to go is refused unless `force`
    /// leaves it behind.
    async fn code_of(
        &self,
        daemon: &dyn Daemon,
        target: &Space,
        into: &Space,
        force: bool,
    ) -> Result<Code> {
        let Some(target) = target.to_workspace() else {
            return Ok(Code::None);
        };
        self.verify_worktree(daemon, &target).await?;
        // A detached HEAD on either side leaves no branch to merge.
        let takes_branch = |into: &Workspace| {
            into.repository() == target.repository()
                && into.branch().is_some()
                && target.branch().is_some()
        };
        if let Some(into) = into.to_workspace().filter(takes_branch) {
            self.verify_worktree(daemon, &into).await?;
            let (target, into) = (Box::new(target), Box::new(into));
            return Ok(Code::Merge { target, into });
        }
        if !self.has_code(daemon, &target).await? {
            return Ok(Code::None);
        }
        ensure!(
            force,
            "{} has commits the default branch lacks or uncommitted files, and no branch of {} in its repository takes them; make it a workspace with shoal convert workspace first, or pass --force to leave the code behind",
            target.name,
            into.label()
        );
        Ok(Code::Discard)
    }

    async fn has_code(&self, daemon: &dyn Daemon, workspace: &Workspace) -> Result<bool> {
        let repo = daemon
            .repository(crate::repository::Selector::from(workspace.repository()))
            .await?;
        let default = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
        let path = &workspace.path;
        let commits = git::status::commits_between(path, &git::local_ref(&default), "HEAD").await?;
        Ok(commits > 0 || !git::status::porcelain(path).await?.is_empty())
    }

    /// The removal takes the spaces `going` with the target. A scoped
    /// `caller` may release only as its `user_holds` permission allows,
    /// asked before anything moves.
    async fn absorb_holds(
        &self,
        daemon: &dyn Daemon,
        target: &Space,
        going: &[SpaceId],
        caller: Option<&Caller>,
    ) -> Result<bool> {
        let Some(caller) = caller else {
            return Ok(true);
        };
        for id in std::iter::once(&target.id).chain(going) {
            if self.user_holds(id).await? {
                daemon
                    .coordination()
                    .authorize_taking_holds(daemon, caller, target, true)
                    .await?;
                return Ok(true);
            }
        }
        Ok(false)
    }
}
