//! Issue and PR actions the daemon carries out for a workspace, so its
//! scope rules apply and each change reaches the event journal.
use anyhow::{Context, Result, bail};

use super::{
    proposals::OpenOutcome,
    wait::{OwnChange, record_own_change},
};
use crate::{
    config::{repo::Opening, resolve::Effective},
    daemon::{events, scope::Caller, workspace::Manager},
    forge::{
        ForgeRepo,
        account::Account,
        conventions,
        items::{
            action::{Action, Edit, Sent},
            create::{NewPull, PullOptions},
            item::{Item, ItemState, Opened},
            link::ItemKind,
        },
    },
    model::{Relation, ReviewMark, Workspace},
    protocol::ItemContext,
};

mod accounts;
mod open_issue;

impl Manager {
    /// Apply `action` to the item `input` names in the repository of
    /// `context`, or to the workspace's linked item of `kind`.
    pub async fn item_action(
        &self,
        context: &ItemContext,
        kind: ItemKind,
        input: Option<String>,
        action: Action,
    ) -> Result<Item> {
        action.validate(kind)?;
        match context {
            ItemContext::Workspace(selector) => {
                self.workspace_item_action(selector, kind, input, action)
                    .await
            }
            ItemContext::Checkout(selector) => {
                let input = input.context(NOTHING_LINKED)?;
                self.checkout_item_action(selector, kind, &input, action)
                    .await
            }
        }
    }

    /// Apply `action` for the workspace and record it as its own change.
    async fn workspace_item_action(
        &self,
        selector: &str,
        kind: ItemKind,
        input: Option<String>,
        action: Action,
    ) -> Result<Item> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let forge = self.workspace_forge(&workspace).await?;
        let input = match input {
            Some(input) => input,
            None => linked(&workspace, kind)?,
        };
        let (number, url) = match kind {
            ItemKind::Issue => forge.issue(&input)?,
            ItemKind::Pr => forge.pull(&input)?,
        };
        let account = if users_decision(&action) {
            self.user_account()
        } else {
            self.agent_account(&workspace).await?
        };
        // Held from before the change, so a poll that observes it records
        // it after the workspace's cursor knows it as its own.
        let _gate = self.gates.lock_prs([&workspace.id]).await;
        let target = Target { kind, number, url };
        self.apply_own(&workspace.id, &forge, &account, target, &action)
            .await
    }

    /// Apply `action` from the repository's registered checkout, which no
    /// workspace watches.
    async fn checkout_item_action(
        &self,
        selector: &str,
        kind: ItemKind,
        input: &str,
        action: Action,
    ) -> Result<Item> {
        let checkout = self.repository(selector).await?;
        let forge = self.checkout_forge(&checkout).await?;
        let (number, url) = match kind {
            ItemKind::Issue => forge.issue(input)?,
            ItemKind::Pr => forge.pull(input)?,
        };
        let account = if users_decision(&action) {
            self.user_account()
        } else {
            self.checkout_agent_account(&checkout).await?
        };
        let acted = forge.apply(&account, kind, number, &action).await;
        Ok(acted
            .with_context(|| format!("could not {} {url}", action.name()))?
            .item)
    }

    /// Move the workspace's open PRs at `urls` into or out of draft as its
    /// agent account, leaving those already in that state unchanged. The
    /// caller holds the workspace's PR gate.
    pub(crate) async fn set_draft(
        &self,
        workspace: &Workspace,
        urls: &[String],
        draft: bool,
    ) -> Result<()> {
        if urls.is_empty() {
            return Ok(());
        }
        let forge = self.workspace_forge(workspace).await?;
        let account = self.agent_account(workspace).await?;
        let action = Action::Edit(Edit {
            draft: Some(draft),
            ..Edit::default()
        });
        for url in urls {
            let (number, url) = forge.pull(url)?;
            let item = forge.read(&account, ItemKind::Pr, number).await?.item;
            let pending = item.state == ItemState::Open
                && item.pr.as_ref().is_some_and(|pr| pr.draft != draft);
            if pending {
                let target = Target {
                    kind: ItemKind::Pr,
                    number,
                    url,
                };
                self.apply_own(&workspace.id, &forge, &account, target, &action)
                    .await?;
            }
        }
        Ok(())
    }

    /// Add to each mark what its item leaves out of the repository's
    /// conventions, read as the workspace's agent account. Its description
    /// was checked when Shoal opened it, so only its labels are.
    pub(crate) async fn check_conventions(
        &self,
        workspace: &Workspace,
        settings: &Effective,
        marks: &mut [ReviewMark],
    ) {
        let Ok((forge, account)) = self.workspace_forge_account(workspace).await else {
            return;
        };
        for mark in marks {
            let Some(item) = marked_item(&forge, &account, mark).await else {
                continue;
            };
            let required = required_labels(settings, item.kind);
            mark.warnings =
                conventions::warnings(&forge, &account, &workspace.path, &item, None, required)
                    .await;
        }
    }

    /// Apply `action` and record it as the workspace's own change, as well as
    /// a comment it posted before failing. The caller holds the workspace's
    /// PR gate.
    pub(super) async fn apply_own(
        &self,
        id: &str,
        forge: &ForgeRepo,
        account: &Account,
        target: Target,
        action: &Action,
    ) -> Result<Item> {
        let acted = match forge
            .apply(account, target.kind, target.number, action)
            .await
        {
            Ok(acted) => acted,
            Err(error) => {
                let error = error.context(format!("could not {} {}", action.name(), target.url));
                return Err(self.record_failed_post(id, target, action, error).await);
            }
        };
        let own = own_state(action).then(|| acted.item.state.as_str());
        let authored = own_posts(action, acted.authored);
        self.record_own_action(id, target, action.name(), own, authored)
            .await
            .map_err(|error| crate::forge::items::action::after_send(error, true))?;
        Ok(acted.item)
    }

    /// Record the comment or reply a failed action posted as the workspace's
    /// own, so polling does not report it back, and return the failure.
    async fn record_failed_post(
        &self,
        id: &str,
        target: Target,
        action: &Action,
        error: anyhow::Error,
    ) -> anyhow::Error {
        let Some(authored) = error
            .downcast_ref::<Sent>()
            .map(|sent| own_posts(action, sent.authored.clone()))
            .filter(|authored| !authored.is_empty())
        else {
            return error;
        };
        match self
            .record_own_action(id, target, action.name(), None, authored)
            .await
        {
            Ok(()) => error,
            Err(recording) => error.context(format!(
                "recording what was posted also failed: {recording:#}"
            )),
        }
    }

    /// Journal an applied action and advance the workspace's cursor past it,
    /// so polling does not report the change back as someone else's.
    async fn record_own_action(
        &self,
        id: &str,
        target: Target,
        name: &'static str,
        state: Option<&'static str>,
        authored: Vec<String>,
    ) -> Result<()> {
        let id = id.to_owned();
        let Target { kind, url, .. } = target;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                events::record_item(&tx, &id, kind, &url, name)?;
                let change = OwnChange {
                    url: &url,
                    authored,
                    state,
                };
                record_own_change(&tx, &id, change)?;
                tx.commit()?;
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

/// Whether the state an action leaves is the agent's own change. Merging is
/// the user's decision, which the agent still hears about.
fn own_state(action: &Action) -> bool {
    matches!(action, Action::Close | Action::Reopen)
}

/// Whether `action` is the user's decision, made as their account: merging,
/// deleting a PR's branch and a review's verdict. Every other change is the
/// agent's.
fn users_decision(action: &Action) -> bool {
    match action {
        Action::Merge { .. }
        | Action::AutoMerge { .. }
        | Action::CancelAutoMerge
        | Action::DeleteBranch => true,
        Action::Review(review) => review.has_verdict(),
        _ => false,
    }
}

/// The activity keys of what `action` posted that are the agent's own, so
/// its watches do not report them back: none of the user's decisions,
/// which the agent still hears about.
fn own_posts(action: &Action, authored: Vec<String>) -> Vec<String> {
    if users_decision(action) {
        Vec::new()
    } else {
        authored
    }
}

impl Manager {
    /// Link the open PR of the workspace's branch, or open one as its agent
    /// account and link it; only a removal that starts meanwhile leaves it
    /// unlinked. A workspace process's new PR is recorded for the user's
    /// approval instead when `[permissions] pr_open` says so.
    pub async fn open_pull(
        &self,
        selector: &str,
        options: PullOptions,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        let proposer = self.proposer(caller).await;
        // Cleanup holds the workspace's gate, so it cannot remove the
        // workspace between opening the PR and linking it.
        let (workspace, _gate) = self.lock_pr_gate(selector).await?;
        self.verify_worktree(&workspace).await?;
        let head = super::pr::current_head(&workspace).await?;
        anyhow::ensure!(
            workspace.state.accepts_links(),
            "workspace cannot accept a PR in its current state"
        );
        let settings = self.workspace_settings(&workspace).await?;
        let required = &settings.pr.required_labels;
        let (forge, account) = self.workspace_forge_account(&workspace).await?;
        let head_repository = head_repository(&workspace.path, &workspace.branch, &forge).await?;
        let existing = forge
            .open_pull_for(&account, &workspace.branch, head_repository.as_deref())
            .await?;
        let (item, body, unfinished) = match existing {
            Some(number) => {
                let item = forge.read(&account, ItemKind::Pr, number).await?.item;
                (item, None, None)
            }
            None => {
                conventions::require(&forge, &account, ItemKind::Pr, required, &options.labels)
                    .await?;
                let pull = NewPull {
                    head_repository,
                    ..new_pull(&workspace, &forge, &account, options, settings.pr.draft).await?
                };
                if let Some(proposer) =
                    proposer.filter(|_| settings.permissions.pr_open == Opening::Propose)
                {
                    forge.check_uploads(&account, &pull.attachments).await?;
                    let proposed = self.propose_pull(&workspace, pull, head, proposer).await?;
                    return Ok(OpenOutcome::Proposed(proposed));
                }
                let created = forge.create_pull(&account, &pull).await?;
                (created.item, Some(pull.body), created.unfinished)
            }
        };
        let created = body.is_some();
        anyhow::ensure!(
            item.pr
                .as_ref()
                .is_some_and(|pr| pr.head == workspace.branch),
            "{} does not have the workspace branch as its head",
            item.url
        );
        let (linked, warning) = if created {
            self.keep_created(&workspace, ItemKind::Pr, &item, true)
                .await?
        } else {
            self.set_pr_gated(&workspace.id, watch(&item), super::pr::WatchCheck::Known)
                .await
                .with_context(|| format!("{} is open, but linking it failed", item.url))?;
            (true, None)
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
        Ok(OpenOutcome::Opened(Opened {
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

    /// Record an item the workspace just created as opened and, with `link`,
    /// assign it. A removal that starts after the caller's check leaves it
    /// unrecorded or unlinked with a warning, so nothing fails once the item
    /// exists and a retry cannot create a duplicate. Returns whether it was
    /// linked, and the warning.
    pub(super) async fn keep_created(
        &self,
        workspace: &Workspace,
        kind: ItemKind,
        item: &Item,
        link: bool,
    ) -> Result<(bool, Option<String>)> {
        let (id, url, title) = (workspace.id.clone(), item.url.clone(), item.title.clone());
        let recorded = self
            .store
            .run(move |db| record_opened(db, &id, kind, &url, &title))
            .await
            .with_context(|| format!("{} was opened, but recording it failed", item.url))?;
        if !recorded || !link {
            return Ok((false, (!recorded).then(|| removal_warning(&item.url))));
        }
        let linked = match kind {
            ItemKind::Issue => {
                let title = Some(item.title.clone());
                self.set_issue_gated(&workspace.id, &item.url, title).await
            }
            ItemKind::Pr => {
                let check = super::pr::WatchCheck::Known;
                self.set_pr_gated(&workspace.id, watch(item), check).await
            }
        };
        match linked {
            Ok(()) => Ok((true, None)),
            Err(_) if !self.accepts_links(&workspace.id).await? => {
                Ok((false, Some(removal_warning(&item.url))))
            }
            Err(error) => {
                Err(error.context(format!("{} was opened, but linking it failed", item.url)))
            }
        }
    }

    /// Whether the workspace still exists and takes link changes.
    async fn accepts_links(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| super::accepts_links(db, &id))
            .await
    }
}

fn watch(item: &Item) -> super::pr::Action {
    super::pr::Action::Watch {
        url: item.url.clone(),
    }
}

/// Record an item the workspace created as opened, with the event of its
/// creation, in one transaction, returning whether it was recorded: a
/// removal that started after the caller's check takes no link changes, and
/// the item exists either way.
fn record_opened(
    db: &mut rusqlite::Connection,
    id: &str,
    kind: ItemKind,
    url: &str,
    title: &str,
) -> Result<bool> {
    let tx = db.transaction()?;
    events::record_item(&tx, id, kind, url, "open")?;
    let recorded = super::accepts_links(&tx, id)?;
    if recorded {
        super::record(&tx, id, Relation::Opened, kind, url, Some(title))?;
    }
    tx.commit()?;
    Ok(recorded)
}

/// The item a mark names as the forge reports it; none for a workspace mark
/// or a failed read.
async fn marked_item(forge: &ForgeRepo, account: &Account, mark: &ReviewMark) -> Option<Item> {
    let (kind, url) = (mark.kind?, mark.url.as_deref()?);
    let (number, _) = match kind {
        ItemKind::Issue => forge.issue(url),
        ItemKind::Pr => forge.pull(url),
    }
    .ok()?;
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
fn removal_warning(url: &str) -> String {
    format!("{url} was opened, but removal started first, so the workspace does not link it")
}

/// Fill what the caller left unset: the title from the linked issue, then
/// the last commit; a closing reference to the linked issue; the base
/// workspace's branch, then the default branch; draft state from
/// `default_draft`.
async fn new_pull(
    workspace: &Workspace,
    forge: &ForgeRepo,
    account: &Account,
    options: PullOptions,
    default_draft: bool,
) -> Result<NewPull> {
    let issue = match &workspace.links.issue {
        Some(url) => Some(forge.issue(url)?.0),
        None => None,
    };
    let title = match (options.title, &workspace.links.issue_title, issue) {
        (Some(title), _, _) => title,
        (None, Some(title), _) => title.clone(),
        (None, None, Some(number)) => {
            forge
                .read(account, ItemKind::Issue, number)
                .await?
                .item
                .title
        }
        (None, None, None) => crate::git::latest_subject(&workspace.path)
            .await?
            .trim()
            .to_owned(),
    };
    let base = match (options.base, &workspace.base_workspace) {
        (Some(base), _) => base,
        (None, Some(base)) => base.branch.clone(),
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
        body: description(options.body, issue),
        head: workspace.branch.clone(),
        head_repository: None,
        base,
        draft: options.draft.unwrap_or(default_draft),
        labels: options.labels,
        reviewers: options.reviewers,
        attachments: options.attachments,
    })
}

/// Use a supplied description verbatim; otherwise close the linked issue.
fn description(body: Option<String>, issue: Option<u64>) -> String {
    let Some(number) = issue else {
        return body.unwrap_or_default();
    };
    let closing = format!("Closes #{number}");
    body.unwrap_or(closing)
}

/// `owner/repository` of the fork the branch's push remote names, or `None`
/// when it pushes to the PR's own repository. A remote is named by where it
/// fetches from, as `origin` is, whatever its push URL.
pub(super) async fn head_repository(
    worktree: &std::path::Path,
    branch: &str,
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

/// The item `input` names, else the workspace's linked one of `kind`;
/// outside a workspace nothing is linked.
pub(super) fn named_or_linked(
    input: Option<String>,
    workspace: Option<&Workspace>,
    kind: ItemKind,
) -> Result<String> {
    match (input, workspace) {
        (Some(input), _) => Ok(input),
        (None, Some(workspace)) => linked(workspace, kind),
        (None, None) => bail!(NOTHING_LINKED),
    }
}

/// Why an action outside a workspace needs its item named.
const NOTHING_LINKED: &str = "outside a workspace, nothing is linked; pass a number or URL";

pub(super) fn linked(workspace: &Workspace, kind: ItemKind) -> Result<String> {
    match kind {
        ItemKind::Issue => workspace
            .links
            .issue
            .clone()
            .context("no issue is linked; pass a number or URL"),
        ItemKind::Pr => match workspace.links.prs.as_slice() {
            [url] => Ok(url.clone()),
            [] => bail!("no PR is linked; pass a number or URL"),
            _ => bail!("several PRs are linked; pass a number or URL"),
        },
    }
}

#[cfg(test)]
mod tests;
