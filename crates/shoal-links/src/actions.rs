//! Issue and PR actions the daemon carries out for a workspace, so its
//! scope rules apply and each change reaches the event journal.
mod accounts;
mod created;
mod open_issue;
mod opening;

use anyhow::{Context, Result};
use shoal_core::config::Effective;
use shoal_daemon_base::{callers::Caller, events, gates::PrGuard};
use shoal_forge::{
    conventions,
    items::{
        action::{ActionRequests, Sent},
        item::ItemRequests,
    },
    transport::account::Account,
    updates::Edited,
};
use shoal_forge_types::{
    ForgeRepo,
    items::{
        action::{Action, Edit},
        item::{Item, ItemState},
        link::ItemKind,
        reference::{ItemInput, ItemRef},
    },
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{ReviewMark, Workspace},
    protocol::RepoTarget,
};
use shoal_refs::BranchName;

use crate::{
    Links,
    daemon::Daemon,
    forges,
    place::{Access, Place},
    space::LinkSpace,
    wait::changes::OwnChange,
};

impl Links {
    /// Apply `action` to the item `input` names in the `target` repository,
    /// or to the workspace's linked item of `kind`.
    pub async fn item_action(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        kind: ItemKind,
        input: Option<ItemInput>,
        action: Action,
        caller: Option<&Caller>,
    ) -> Result<Item> {
        action.validate(kind)?;
        let targeted = self.target_item(daemon, target, input, kind, Access::Change, caller);
        let (target, item) = targeted.await?;
        let mut action = action;
        configure_merge(daemon, target.own_place(), &mut action).await?;
        let forge = target.forge()?;
        match target.own_place() {
            Some(Place::Space(space)) => {
                let acted =
                    self.space_item_action(daemon, space, forge, kind, &item, action, caller);
                acted.await
            }
            place => {
                let repository = place.map(Place::repository_id);
                (self.user_item_action(daemon, repository, forge, kind, &item, action)).await
            }
        }
    }

    /// Apply `action` to `item` of the workspace's or chat's repository,
    /// `forge`, and record it as the space's own change: the user's
    /// decisions with the user's login, unless a scoped `caller` takes one
    /// as its permissions allow, as the review account for a review's
    /// verdict and the agent account otherwise.
    #[allow(
        clippy::too_many_arguments,
        reason = "the action and the item it acts on, beside the daemon and its caller"
    )]
    async fn space_item_action(
        &self,
        daemon: &dyn Daemon,
        space: &LinkSpace,
        forge: &ForgeRepo,
        kind: ItemKind,
        item: &ItemRef,
        action: Action,
        caller: Option<&Caller>,
    ) -> Result<Item> {
        let (number, url) = (item.number().get(), item.to_string());
        let account = match caller {
            None if users_decision(&action) => self.forges.user_account(),
            Some(_) if has_verdict(&action) => self.forges.review_account(),
            _ => self.forges.agent_account(),
        };
        // Held from before the change, so a poll that observes it records
        // it after the cursors that take it as the space's own know it.
        let _gate = self.lock_change_gates(&space.id, &[]).await?;
        let target = Target { kind, number, url };
        let item = self
            .apply_own(daemon, &space.id, forge, &account, target, &action, caller)
            .await?;
        sync_after_merge(daemon, space.repository_id(), &action, &item);
        Ok(item)
    }

    /// Apply `action` to `item` with the user's own logins, from the
    /// registered checkout of `repository`, which no workspace watches, or
    /// in another repository than the request's own: only the user acts
    /// there.
    async fn user_item_action(
        &self,
        daemon: &dyn Daemon,
        repository: Option<&RepositoryId>,
        forge: &ForgeRepo,
        kind: ItemKind,
        item: &ItemRef,
        action: Action,
    ) -> Result<Item> {
        let number = item.number().get();
        let changed = forge
            .apply(&self.forges.user_account(), kind, number, &action)
            .await
            .with_context(|| format!("could not {} {item}", action.verb()))?
            .item;
        if let Some(repository) = repository {
            sync_after_merge(daemon, repository, &action, &changed);
        }
        Ok(changed)
    }

    /// Hold the PR gates of the space `id`, of every space below it and of
    /// `others`. A change the space makes through Shoal is recorded on its
    /// cursor and on those of the spaces below it whose activity it receives
    /// when the change is recorded, so holding them all from before the
    /// change keeps a poll from recording it first, whichever agents start
    /// or stop meanwhile.
    pub async fn lock_change_gates(&self, id: &SpaceId, others: &[&SpaceId]) -> Result<PrGuard> {
        let owned = id.to_owned();
        let subtree = (self.store)
            .read(move |db| shoal_store::tree::subtree(db, &owned))
            .await?;
        let ids = subtree.iter().chain(others.iter().copied());
        Ok(self.gates.lock_prs(ids).await)
    }

    /// Move the space's open PRs at `urls` into or out of draft as its
    /// agent account, leaving those already in that state unchanged. The
    /// caller holds the gates [`Self::lock_change_gates`] takes for the
    /// space.
    pub async fn set_draft(
        &self,
        daemon: &dyn Daemon,
        space: &LinkSpace,
        urls: &[ItemRef],
        draft: bool,
    ) -> Result<()> {
        if urls.is_empty() {
            return Ok(());
        }
        let forge = forges::of_space(&self.forges, space).await?;
        let account = self.forges.agent_account();
        let action = Action::edit(Edit {
            draft: Some(draft),
            ..Edit::default()
        });
        for url in urls {
            url.belongs_to(&forge)?;
            let (number, url) = (url.pull()?, url.to_string());
            let item = forge.read(&account, ItemKind::Pr, number.get()).await?.item;
            let pending = item.state == ItemState::Open
                && item.pr.as_ref().is_some_and(|pr| pr.draft != draft);
            if pending {
                let target = Target {
                    kind: ItemKind::Pr,
                    number: number.get(),
                    url,
                };
                (self.apply_own(daemon, &space.id, &forge, &account, target, &action, None))
                    .await?;
            }
        }
        Ok(())
    }

    /// Add to each mark what its item leaves out of the repository's
    /// conventions, read as the workspace's agent account. Its description
    /// was checked when Shoal opened it, so only its labels are.
    pub async fn check_conventions(
        &self,
        space: &LinkSpace,
        settings: &Effective,
        marks: &mut [ReviewMark],
    ) {
        let Ok(forge) = forges::of_space(&self.forges, space).await else {
            return;
        };
        let account = self.forges.agent_account();
        for mark in marks {
            let Some(item) = marked_item(&forge, &account, mark).await else {
                continue;
            };
            let required = required_labels(settings, item.kind);
            mark.warnings =
                conventions::warnings(&forge, &account, space.origin_path(), &item, None, required)
                    .await;
        }
    }

    /// Apply `action` and record it as the workspace's own change, as well as
    /// a comment it posted before failing; `caller` takes it, `None` for the
    /// user and for Shoal's own changes. A PR it moves into or out of draft
    /// has its progress section published again. The caller holds the gates
    /// [`Self::lock_change_gates`] takes for the workspace.
    #[allow(
        clippy::too_many_arguments,
        reason = "the action, its item and its account, beside the daemon and its caller"
    )]
    pub async fn apply_own(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        forge: &ForgeRepo,
        account: &Account,
        target: Target,
        action: &Action,
        caller: Option<&Caller>,
    ) -> Result<Item> {
        let acted = match forge
            .apply(account, target.kind, target.number, action)
            .await
        {
            Ok(acted) => acted,
            Err(error) => {
                let error = error.context(format!("could not {} {}", action.verb(), target.url));
                let failed = self.record_failed_post(daemon, id, target, action, caller, error);
                return Err(failed.await);
            }
        };
        let changed_draft = acted.edited.changed_draft();
        let own = Own {
            state: own_state(action).then_some(acted.item.state),
            authored: own_posts(action, caller, acted.authored),
            edited: own_edits(action, acted.edited),
        };
        self.record_own_action(daemon, id, target, action.name(), own)
            .await
            .map_err(|error| shoal_forge::items::action::after_send(error, true))?;
        if changed_draft {
            daemon.draft_changed(id);
        }
        Ok(acted.item)
    }

    /// Record the comment or reply a failed action posted as the workspace's
    /// own, so polling does not report it back, and return the failure.
    async fn record_failed_post(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        target: Target,
        action: &Action,
        caller: Option<&Caller>,
        error: anyhow::Error,
    ) -> anyhow::Error {
        let Some(authored) = error
            .downcast_ref::<Sent>()
            .map(|sent| own_posts(action, caller, sent.authored.clone()))
            .filter(|authored| !authored.is_empty())
        else {
            return error;
        };
        let own = Own {
            state: None,
            authored,
            edited: Edited::default(),
        };
        match (self.record_own_action(daemon, id, target, action.name(), own)).await {
            Ok(()) => error,
            Err(recording) => error.context(format!(
                "recording what was posted also failed: {recording:#}"
            )),
        }
    }

    /// Journal an applied action and advance the cursors of the workspace
    /// and of the spaces below it whose activity it receives past it, so
    /// polling does not report the change back as someone else's.
    async fn record_own_action(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        target: Target,
        name: &'static str,
        own: Own,
    ) -> Result<()> {
        let id = id.to_owned();
        let (agents, records) = (daemon.tracked_agents().await, self.records.clone());
        let Target { kind, url, .. } = target;
        let Own {
            state,
            authored,
            edited,
        } = own;
        self.store
            .write(move |tx| {
                events::record_item(tx, &id, kind, &url, name)?;
                let change = OwnChange {
                    url: &url,
                    authored,
                    state,
                    edited,
                };
                crate::wait::activity::record_received_change(&*records, tx, &id, change, &agents)
            })
            .await
    }
}

/// Have `action`, when it is a merge that names no method, merge by the
/// `[merge] method` of `place`, the place it acts in as its own repository,
/// or of the global config elsewhere.
pub async fn configure_merge(
    daemon: &dyn Daemon,
    place: Option<&Place>,
    action: &mut Action,
) -> Result<()> {
    if !action.merges() {
        return Ok(());
    }
    let settings = match place {
        Some(Place::Space(space)) => daemon.acting_settings(space).await?,
        Some(place) => daemon.repository_settings(place.repository_id()).await?,
        None => daemon.settings_in(None).await?,
    };
    if let Some(method) = settings.merge.method {
        action.default_merge_method(method);
    }
    Ok(())
}

/// Have the daemon sync the repository a merge just changed, without
/// waiting for it, so conflict checks of its other workspaces see the
/// merge.
fn sync_after_merge(
    daemon: &dyn Daemon,
    repository_id: &RepositoryId,
    action: &Action,
    item: &Item,
) {
    if action.merges() && item.state == ItemState::Merged {
        daemon.item_merged(repository_id);
    }
}

/// The item an action changes.
#[derive(Clone)]
pub struct Target {
    pub kind: ItemKind,
    pub number: u64,
    pub url: String,
}

/// What an applied action changed that is the agent's own.
struct Own {
    state: Option<ItemState>,
    authored: Vec<String>,
    edited: Edited,
}

/// Whether the state an action leaves is the agent's own change. Merging is
/// the user's decision, which the agent still hears about.
fn own_state(action: &Action) -> bool {
    matches!(action, Action::Close | Action::Reopen)
}

/// Whether `action` is the user's decision, made as their account: merging
/// and a review's verdict. Every other change is the agent's.
fn users_decision(action: &Action) -> bool {
    match action {
        Action::Merge { .. } | Action::AutoMerge { .. } | Action::CancelAutoMerge => true,
        _ => has_verdict(action),
    }
}

/// Whether `action` is a review that approves or requests changes.
fn has_verdict(action: &Action) -> bool {
    matches!(action, Action::Review(review) if review.has_verdict())
}

/// The activity keys of what `action`, which `caller` took, posted that are
/// the agent's own, so its watches do not report them back: none of the
/// user's decisions, which the agent still hears about, but a scoped
/// caller's verdict.
fn own_posts(action: &Action, caller: Option<&Caller>, authored: Vec<String>) -> Vec<String> {
    if caller.is_none() && users_decision(action) {
        Vec::new()
    } else {
        authored
    }
}

/// What `action` changed of the item's details that is the agent's own:
/// only an edit's, since what another action's reads show changed is
/// someone else's change.
fn own_edits(action: &Action, edited: Edited) -> Edited {
    if matches!(action, Action::Edit(_)) {
        edited
    } else {
        Edited::default()
    }
}

/// Refuse a PR whose head is not the workspace branch.
pub fn ensure_branch_head(workspace: &Workspace, item: &Item) -> Result<()> {
    anyhow::ensure!(
        item.pr
            .as_ref()
            .is_some_and(|pr| workspace.branch().map(BranchName::as_str) == Some(pr.head.as_str())),
        "{} does not have the workspace branch as its head",
        item.url
    );
    Ok(())
}

/// `owner/repository` of the fork the branch's push remote names, or `None`
/// when it pushes to the PR's own repository. A remote is named by where it
/// fetches from, as `origin` is, whatever its push URL.
pub async fn head_repository(
    worktree: &std::path::Path,
    branch: &BranchName,
    forge: &ForgeRepo,
) -> Result<Option<String>> {
    let remote = shoal_core::git::push_remote(worktree, branch).await?;
    let url = shoal_core::git::remote_url(worktree, &remote).await?;
    let pushed = ForgeRepo::parse(url.trim())
        .with_context(|| format!("the branch's push remote {remote} is not a forge repository"))?;
    if pushed == *forge {
        return Ok(None);
    }
    anyhow::ensure!(
        pushed.host == forge.host,
        "the branch's push remote {remote} is on {}, so a PR on {} cannot have it as its head",
        pushed.host,
        forge.host
    );
    Ok(Some(pushed.path))
}

/// The item a mark names as the forge reports it; none for a workspace mark
/// or a failed read.
async fn marked_item(forge: &ForgeRepo, account: &Account, mark: &ReviewMark) -> Option<Item> {
    let (kind, url) = (mark.kind?, mark.url.as_ref()?);
    url.belongs_to(forge).ok()?;
    let number = url.number().get();
    Some(forge.read(account, kind, number).await.ok()?.item)
}

/// The labels `[pr]` or `[issue]` requires of an item of `kind`.
pub fn required_labels(settings: &Effective, kind: ItemKind) -> &[String] {
    match kind {
        ItemKind::Issue => &settings.issue.required_labels,
        ItemKind::Pr => &settings.pr.required_labels,
    }
}

#[cfg(test)]
mod tests;
