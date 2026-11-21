//! Issue and PR actions the daemon carries out for a workspace, so its
//! scope rules apply and each change reaches the event journal.
use crate::daemon::links::proposals::OpenOutcomeExt;
use anyhow::{Context, Result};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_links::issue::assignees::AssignerOf;
use shoal_links::place::{Access, Place};
use shoal_links::space::LinkSpace;
use shoal_links::wait::changes::OwnChange;
use shoal_refs::BranchName;

use super::{issue::Assigner, proposals::OpenOutcome};
use crate::{
    config::{permissions::Opening, resolve::Effective},
    daemon::{events, manager::Manager, scope::Caller},
    forge::{
        ForgeRepo,
        items::{
            action::{Action, Edit},
            create::{NewPull, PullOptions},
            item::{Item, ItemState, Opened},
            link::ItemKind,
            reference::{ItemInput, ItemRef},
        },
    },
    model::{Relation, ReviewMark, Workspace},
    protocol::RepoTarget,
};
use shoal_forge::{
    account::Account,
    conventions,
    items::{
        action::{ActionRequests, Sent},
        attach::AttachRequests,
        create::CreateRequests,
    },
    updates::Edited,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_reviews::required::Gate;

mod accounts;
mod open_issue;

impl Manager {
    /// Apply `action` to the item `input` names in the `target` repository,
    /// or to the workspace's linked item of `kind`.
    pub async fn item_action(
        &self,
        target: &RepoTarget,
        kind: ItemKind,
        input: Option<ItemInput>,
        action: Action,
        caller: Option<&Caller>,
    ) -> Result<Item> {
        action.validate(kind)?;
        let (target, item) =
            (self
                .links
                .target_item(self, target, input, kind, Access::Change, caller))
            .await?;
        let mut action = action;
        self.configure_merge(target.own_place(), &mut action)
            .await?;
        let forge = target.forge()?;
        match target.own_place() {
            Some(Place::Space(space)) => {
                (self.space_item_action(space, forge, kind, &item, action, caller)).await
            }
            place => {
                let repository = place.map(Place::repository_id);
                (self.user_item_action(repository, forge, kind, &item, action)).await
            }
        }
    }

    /// Have `action`, when it is a merge that names no method, merge by the
    /// `[merge] method` of `place`, the place it acts in as its own
    /// repository, or of the global config elsewhere.
    pub(super) async fn configure_merge(
        &self,
        place: Option<&Place>,
        action: &mut Action,
    ) -> Result<()> {
        if !action.merges() {
            return Ok(());
        }
        let settings = match place {
            Some(Place::Space(space)) => self.acting_settings(space).await?,
            Some(place) => self.repository_settings(place.repository_id()).await?,
            None => self.space_settings_in(None).await?,
        };
        if let Some(method) = settings.merge.method {
            action.default_merge_method(method);
        }
        Ok(())
    }

    /// Apply `action` to `item` of the workspace's or chat's repository,
    /// `forge`, and record it as the space's own change: the user's
    /// decisions with the user's login, unless a scoped `caller` takes one
    /// as its permissions allow, as the review account for a review's
    /// verdict and the agent account otherwise.
    async fn space_item_action(
        &self,
        space: &LinkSpace,
        forge: &ForgeRepo,
        kind: ItemKind,
        item: &ItemRef,
        action: Action,
        caller: Option<&Caller>,
    ) -> Result<Item> {
        let (number, url) = (item.number().get(), item.to_string());
        let account = match caller {
            None if users_decision(&action) => self.links.user_account(),
            Some(_) if has_verdict(&action) => self.links.review_account(),
            _ => self.links.agent_account(),
        };
        // Held from before the change, so a poll that observes it records
        // it after the cursors that take it as the space's own know it.
        let _gate = self.lock_change_gates(&space.id, &[]).await?;
        let target = Target { kind, number, url };
        let item = self
            .apply_own(&space.id, forge, &account, target, &action, caller)
            .await?;
        self.sync_after_merge(space.repository_id(), &action, &item);
        Ok(item)
    }

    /// Apply `action` to `item` with the user's own logins, from the
    /// registered checkout of `repository`, which no workspace watches, or
    /// in another repository than the request's own: only the user acts
    /// there.
    async fn user_item_action(
        &self,
        repository: Option<&RepositoryId>,
        forge: &ForgeRepo,
        kind: ItemKind,
        item: &ItemRef,
        action: Action,
    ) -> Result<Item> {
        let number = item.number().get();
        let changed = forge
            .apply(&self.links.user_account(), kind, number, &action)
            .await
            .with_context(|| format!("could not {} {item}", action.verb()))?
            .item;
        if let Some(repository) = repository {
            self.sync_after_merge(repository, &action, &changed);
        }
        Ok(changed)
    }

    /// Sync the repository a merge just changed, without waiting for it, so
    /// conflict checks of its other workspaces see the merge.
    fn sync_after_merge(&self, repository_id: &RepositoryId, action: &Action, item: &Item) {
        if action.merges() && item.state == ItemState::Merged {
            self.syncs.request(repository_id);
        }
    }

    /// Move the space's open PRs at `urls` into or out of draft as its
    /// agent account, leaving those already in that state unchanged. The
    /// caller holds the gates [`Manager::lock_change_gates`] takes for the
    /// space.
    pub(crate) async fn set_draft(
        &self,
        space: &LinkSpace,
        urls: &[ItemRef],
        draft: bool,
    ) -> Result<()> {
        if urls.is_empty() {
            return Ok(());
        }
        let forge = shoal_links::forges::of_space(self, space).await?;
        let account = self.links.agent_account();
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
                self.apply_own(&space.id, &forge, &account, target, &action, None)
                    .await?;
            }
        }
        Ok(())
    }

    /// Show `section` as the progress section of the space's PR at `url`
    /// while it is a draft, or remove its section for `None` or a PR out of
    /// draft, as the agent account, unless the PR is no longer open or
    /// already shows it. The caller holds the gates
    /// [`Manager::lock_change_gates`] takes for the space.
    pub(crate) async fn write_progress_section(
        &self,
        space: &LinkSpace,
        pull: &ItemRef,
        section: Option<&str>,
    ) -> Result<()> {
        let forge = shoal_links::forges::of_space(self, space).await?;
        let account = self.links.agent_account();
        pull.belongs_to(&forge)?;
        let (number, url) = (pull.pull()?, pull.to_string());
        let current = forge.read(&account, ItemKind::Pr, number.get()).await?;
        if current.item.state != ItemState::Open {
            return Ok(());
        }
        let draft = current.item.pr.as_ref().is_some_and(|pr| pr.draft);
        let section = section.filter(|_| draft);
        let body = crate::daemon::progress::section::with_section(&current.body, section);
        if body == current.body {
            return Ok(());
        }
        let action = Action::edit(Edit {
            body: Some(body),
            ..Edit::default()
        });
        let target = Target {
            kind: ItemKind::Pr,
            number: number.get(),
            url,
        };
        self.apply_own(&space.id, &forge, &account, target, &action, None)
            .await?;
        Ok(())
    }

    /// Add to each mark what its item leaves out of the repository's
    /// conventions, read as the workspace's agent account. Its description
    /// was checked when Shoal opened it, so only its labels are.
    pub(crate) async fn check_conventions(
        &self,
        space: &LinkSpace,
        settings: &Effective,
        marks: &mut [ReviewMark],
    ) {
        let Ok(forge) = shoal_links::forges::of_space(self, space).await else {
            return;
        };
        let account = self.links.agent_account();
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
    /// [`Manager::lock_change_gates`] takes for the workspace.
    pub(super) async fn apply_own(
        &self,
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
                let failed = self.record_failed_post(id, target, action, caller, error);
                return Err(failed.await);
            }
        };
        let changed_draft = acted.edited.changed_draft();
        let own = Own {
            state: own_state(action).then_some(acted.item.state),
            authored: own_posts(action, caller, acted.authored),
            edited: own_edits(action, acted.edited),
        };
        self.record_own_action(id, target, action.name(), own)
            .await
            .map_err(|error| shoal_forge::items::action::after_send(error, true))?;
        if changed_draft {
            self.publications.request(id);
        }
        Ok(acted.item)
    }

    /// Record the comment or reply a failed action posted as the workspace's
    /// own, so polling does not report it back, and return the failure.
    async fn record_failed_post(
        &self,
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
        match self.record_own_action(id, target, action.name(), own).await {
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
        id: &SpaceId,
        target: Target,
        name: &'static str,
        own: Own,
    ) -> Result<()> {
        let id = id.to_owned();
        let (agents, links) = (self.tracked_agents().await, self.links.clone());
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
                links.record_received_change(tx, &id, change, agents.ids())?;
                Ok(())
            })
            .await
    }
}

/// The item an action changes.
#[derive(Clone)]
pub(super) struct Target {
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
fn ensure_branch_head(workspace: &Workspace, item: &Item) -> Result<()> {
    anyhow::ensure!(
        item.pr
            .as_ref()
            .is_some_and(|pr| workspace.branch().map(BranchName::as_str) == Some(pr.head.as_str())),
        "{} does not have the workspace branch as its head",
        item.url
    );
    Ok(())
}

impl Manager {
    /// Link the open PR of the workspace's branch, or open one as its agent
    /// account and link it; only a removal that starts meanwhile leaves it
    /// unlinked. A workspace process's new PR is recorded for the user's
    /// approval instead when its `pr_open` permission says so, or when
    /// `push_held` says the `push` permission held back the branch's push,
    /// which approving it runs first; a held push to an open PR is recorded
    /// on its own.
    pub async fn open_pull(
        &self,
        selector: &SpaceSelector,
        options: PullOptions,
        push_held: bool,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        let proposer = self.proposer(caller).await;
        // Cleanup holds the workspace's gate, so it cannot remove the
        // workspace between opening the PR and linking it.
        let (workspace, _gate) = self.lock_pr_gate(selector).await?;
        let branch = workspace.require_branch("shoal pr open")?.clone();
        self.verify_worktree(&workspace).await?;
        let head = shoal_links::pr::current_head(&workspace).await?;
        let push = match push_held {
            true => Some(
                self.held_push(&workspace, proposer.as_ref(), caller)
                    .await?,
            ),
            false => None,
        };
        anyhow::ensure!(
            workspace.state.accepts_links(),
            "workspace cannot accept a PR in its current state"
        );
        let settings = self.workspace_settings(&workspace).await?;
        let required = &settings.pr.required_labels;
        let (forge, account) = self.workspace_forge_account(&workspace).await?;
        let head_repository = head_repository(&workspace.path, &branch, &forge).await?;
        let existing = forge
            .open_pull_for(&account, &branch, head_repository.as_deref())
            .await?;
        let (item, body, unfinished) = match existing {
            Some(number) => {
                let item = forge.read(&account, ItemKind::Pr, number).await?.item;
                // The PR is linked now; approving the push updates it.
                if let Some(push) = push {
                    self.link_open_pull(&workspace, &item).await?;
                    self.ensure_workspace_not_pushing(&workspace).await?;
                    let proposed = self.propose_push(&workspace, push).await?;
                    return Ok(OpenOutcome::Proposed(proposed));
                }
                (item, None, None)
            }
            None => {
                self.reviews
                    .require(self, &workspace, Gate::Pr, &head, caller)
                    .await?;
                conventions::require(&forge, &account, ItemKind::Pr, required, &options.labels)
                    .await?;
                let pull = NewPull {
                    head_repository,
                    ..new_pull(&workspace, &forge, &account, options, settings.pr.draft).await?
                };
                // A PR whose branch is not pushed cannot open before the
                // push is approved, which then opens it too.
                let proposes = push.is_some() || self.proposes_pull(&workspace, caller).await?;
                if let Some(proposer) = proposer.filter(|_| proposes) {
                    forge.check_uploads(&account, &pull.attachments).await?;
                    self.ensure_workspace_not_pushing(&workspace).await?;
                    let proposed = self
                        .propose_pull(&workspace, pull, head, push, proposer)
                        .await?;
                    return Ok(OpenOutcome::Proposed(proposed));
                }
                let created = forge.create_pull(&account, &pull).await?;
                (created.item, Some(pull.body), created.unfinished)
            }
        };
        let created = body.is_some();
        let (linked, warning) = if created {
            ensure_branch_head(&workspace, &item)?;
            (self.keep_created(
                &workspace.id,
                ItemKind::Pr,
                &item,
                true,
                Assigner::of(caller),
            ))
            .await?
        } else {
            self.link_open_pull(&workspace, &item).await?;
            (true, Vec::new())
        };
        let checked = conventions::warnings(
            &forge,
            &account,
            &workspace.path,
            &item,
            body.as_deref(),
            required,
        )
        .await;
        Ok(OpenOutcome::opened(Opened {
            item,
            created,
            linked,
            warnings: warning
                .into_iter()
                .chain(unfinished)
                .chain(checked)
                .collect(),
        }))
    }

    /// Link `item`, an open PR the workspace did not create, after checking
    /// that its head is the workspace branch.
    pub(super) async fn link_open_pull(&self, workspace: &Workspace, item: &Item) -> Result<()> {
        ensure_branch_head(workspace, item)?;
        let url = item.url.clone();
        self.links
            .set_pr_gated(
                self,
                &workspace.clone().into(),
                super::pr::Action::Watch {
                    url: url.into(),
                    title: Some(item.title.clone()),
                },
                shoal_links::pr::WatchCheck::Known,
            )
            .await
            .with_context(|| format!("{} is open, but linking it failed", item.url))
    }

    /// Whether `caller`'s `shoal pr open` would open a new PR that its
    /// review gate refuses; checked before anything is pushed.
    pub(crate) async fn check_pr_review_gate(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        let Some(branch) = workspace.branch().cloned() else {
            return Ok(());
        };
        let settings = self.workspace_settings(&workspace).await?;
        if !Gate::Pr.holds(&settings, caller) {
            return Ok(());
        }
        let (forge, account) = self.workspace_forge_account(&workspace).await?;
        let head_repository = head_repository(&workspace.path, &branch, &forge).await?;
        let existing = forge
            .open_pull_for(&account, &branch, head_repository.as_deref())
            .await?;
        match existing {
            Some(_) => Ok(()),
            None => {
                let head = shoal_links::pr::current_head(&workspace).await?;
                self.reviews
                    .require(self, &workspace, Gate::Pr, &head, caller)
                    .await
            }
        }
    }

    /// Assign an item the workspace just created with `link`, and record it
    /// as opened without, or when assigning it fails. A removal that starts
    /// after the caller's check leaves it unrecorded with a warning, so
    /// nothing fails once the item exists and a retry cannot create a
    /// duplicate. An assigned issue is assigned on the forge to the account
    /// of `assigner`. Returns whether it was assigned, and the warnings.
    pub(super) async fn keep_created(
        &self,
        space: &SpaceId,
        kind: ItemKind,
        item: &Item,
        link: bool,
        assigner: Assigner,
    ) -> Result<(bool, Vec<String>)> {
        self.journal_created(space, kind, item).await?;
        let failure = match link {
            true => match self.assign_created(space, kind, item, assigner).await {
                Ok(warnings) => return Ok((true, warnings)),
                Err(error) => Some(error),
            },
            false => None,
        };
        if !self.record_created(space, kind, item).await? {
            return Ok((false, vec![removal_warning(&item.url)]));
        }
        match failure {
            Some(error) => {
                Err(error.context(format!("{} was opened, but linking it failed", item.url)))
            }
            None => Ok((false, Vec::new())),
        }
    }

    /// Journal that the space created `item`.
    async fn journal_created(&self, space: &SpaceId, kind: ItemKind, item: &Item) -> Result<()> {
        let (id, url) = (space.to_owned(), item.url.clone());
        self.store
            .write(move |tx| events::record_item(tx, &id, kind, url.url(), "open"))
            .await
            .with_context(|| format!("{} was opened, but recording it failed", item.url))
    }

    /// Assign `item`, which the space just created, returning what
    /// assigning an issue on the forge warns of.
    async fn assign_created(
        &self,
        space: &SpaceId,
        kind: ItemKind,
        item: &Item,
        assigner: Assigner,
    ) -> Result<Vec<String>> {
        let url = item.url.clone();
        match kind {
            ItemKind::Issue => {
                let (selector, title) = (SpaceSelector::id(space), Some(item.title.clone()));
                (self
                    .links
                    .set_issue_gated(self, &selector, &url.into(), title, assigner))
                .await
            }
            ItemKind::Pr => {
                let check = shoal_links::pr::WatchCheck::Known;
                let title = Some(item.title.clone());
                let watch = super::pr::Action::Watch {
                    url: url.into(),
                    title,
                };
                let space = self
                    .links
                    .link_space(self, &SpaceSelector::id(space))
                    .await?;
                self.links.set_pr_gated(self, &space, watch, check).await?;
                Ok(Vec::new())
            }
        }
    }

    /// Record `item`, which the space just created, as opened, returning
    /// whether it was recorded: a removal that started after the caller's
    /// check takes no link changes, and the item exists either way.
    async fn record_created(&self, space: &SpaceId, kind: ItemKind, item: &Item) -> Result<bool> {
        let (id, url, title) = (space.to_owned(), item.url.clone(), item.title.clone());
        let links = self.links.clone();
        self.store
            .write(move |tx| record_opened(&links, tx, &id, kind, url.url(), &title))
            .await
            .with_context(|| format!("{} was opened, but recording it failed", item.url))
    }
}

/// Record an item the workspace created as opened in one transaction,
/// returning whether it was recorded, which a removal that started after the
/// caller's check prevents.
fn record_opened(
    links: &shoal_links::Links,
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    kind: ItemKind,
    url: &str,
    title: &str,
) -> Result<bool> {
    let recorded = crate::daemon::links::accepts_links(tx, id)?;
    if recorded {
        links.record(tx, id, Relation::Opened, kind, url, Some(title))?;
    }
    Ok(recorded)
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
pub(super) fn required_labels(settings: &Effective, kind: ItemKind) -> &[String] {
    match kind {
        ItemKind::Issue => &settings.issue.required_labels,
        ItemKind::Pr => &settings.pr.required_labels,
    }
}

/// The warning for an item created after removal started.
fn removal_warning(url: &ItemRef) -> String {
    format!("{url} was opened, but removal started first, so the workspace does not link it")
}

/// Fill what the caller left unset: the title from the first linked issue,
/// then the last commit; a closing reference to each linked issue; the base
/// workspace's branch, then the default branch; draft state from
/// `default_draft`.
async fn new_pull(
    workspace: &Workspace,
    forge: &ForgeRepo,
    account: &Account,
    options: PullOptions,
    default_draft: bool,
) -> Result<NewPull> {
    let issues = (workspace.links.issues.iter())
        .map(|linked| {
            linked.url.belongs_to(forge)?;
            linked.url.issue()
        })
        .collect::<Result<Vec<_>>>()?;
    let linked_title = (workspace.links.issue()).and_then(|linked| linked.title.clone());
    let title = match (options.title, linked_title, issues.first()) {
        (Some(title), _, _) => title,
        (None, Some(title), _) => title,
        (None, None, Some(number)) => {
            forge
                .read(account, ItemKind::Issue, number.get())
                .await?
                .item
                .title
        }
        (None, None, None) => crate::git::latest_subject(&workspace.path)
            .await?
            .trim()
            .to_owned(),
    };
    let base_branch = workspace
        .base_workspace
        .as_ref()
        .and_then(|base| base.branch.clone());
    let base = match (options.base, base_branch) {
        (Some(base), _) => base,
        (None, Some(branch)) => branch,
        (None, None) => {
            crate::git::default_branch::resolve(
                &workspace.path,
                crate::git::default_branch::DefaultBranchLookup::Discover,
            )
            .await?
        }
    };
    Ok(NewPull {
        title,
        body: conventions::pull_description(options.body, &issues),
        head: workspace.require_branch("shoal pr open")?.clone(),
        head_repository: None,
        base,
        draft: options.draft.unwrap_or(default_draft),
        labels: options.labels,
        reviewers: options.reviewers,
        assignees: options.assignees,
        milestone: options.milestone,
        attachments: options.attachments,
    })
}

/// `owner/repository` of the fork the branch's push remote names, or `None`
/// when it pushes to the PR's own repository. A remote is named by where it
/// fetches from, as `origin` is, whatever its push URL.
pub(super) async fn head_repository(
    worktree: &std::path::Path,
    branch: &BranchName,
    forge: &ForgeRepo,
) -> Result<Option<String>> {
    let remote = crate::git::push_remote(worktree, branch).await?;
    let url = crate::git::remote_url(worktree, &remote).await?;
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

impl Manager {
    /// Whether a scoped `caller`'s `pr_open` permission has it propose the
    /// new PR of `workspace` instead of opening it; unscoped callers open.
    async fn proposes_pull(&self, workspace: &Workspace, caller: Option<&Caller>) -> Result<bool> {
        let Some(caller) = caller else {
            return Ok(false);
        };
        let permissions = self
            .caller_permissions(caller, Some(workspace.repository()))
            .await?;
        Ok(permissions.pr_open.value != Opening::Direct)
    }
}

#[cfg(test)]
mod tests;
