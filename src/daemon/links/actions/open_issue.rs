//! Opening an issue for a workspace or a chat, or from a registered
//! checkout, in its repository or another one, or recording it for the
//! user's approval where the repository asks for that.
use std::path::Path;

use anyhow::{Context as _, Result, bail};

use crate::{
    config::{
        repo::{Opening, OtherRepositories},
        resolve::Effective,
    },
    daemon::{
        links::{
            LinkSpace,
            proposals::{OpenOutcome, Proposer},
        },
        scope::Caller,
        workspace::Manager,
    },
    forge::{
        ForgeRepo,
        account::Account,
        conventions,
        items::{
            create::{Created, NewIssue},
            item::{Item, Opened},
            link::ItemKind,
        },
    },
    protocol::ItemContext,
};

/// Why `--link` is refused outside a workspace or chat.
const LINK_NEEDS_SPACE: &str =
    "--link needs a workspace or chat to assign the issue to; run this in one or pass --space";
/// Why `--link` is refused in a chat without a repository.
const LINK_NEEDS_REPOSITORY: &str =
    "a chat without a repository links no issues; open the issue without --link";

impl Manager {
    /// Open an issue as the agent account in the repository of `context`, or
    /// the one `repository` names, and link it to the workspace when `link`
    /// is set.
    pub async fn open_issue(
        &self,
        context: &ItemContext,
        issue: NewIssue,
        repository: Option<String>,
        link: bool,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        anyhow::ensure!(!issue.title.trim().is_empty(), "the issue needs a title");
        match context {
            ItemContext::Workspace(selector) => {
                self.open_workspace_issue(selector, issue, repository, link, caller)
                    .await
            }
            ItemContext::Chat(_) | ItemContext::ChatIn { .. } => {
                self.open_chat_issue(context, issue, repository, link, caller)
                    .await
            }
            ItemContext::Checkout(selector) => {
                anyhow::ensure!(!link, "{LINK_NEEDS_SPACE}");
                self.open_checkout_issue(selector, issue, repository).await
            }
        }
    }

    /// Open the issue from the checkout of the chat `context` names, its
    /// repository's or the one a chat without a repository acts in, as a
    /// checkout opens one, and record it as opened by the chat, or assign it
    /// to the chat with `link`. The chat's own agent, a scoped `caller`,
    /// records it for the user's approval instead where `[permissions]` says
    /// so, as a workspace's does. A chat without a repository acting in no
    /// registered one opens it as [`Self::open_outside_issue`] does, and
    /// records nothing.
    async fn open_chat_issue(
        &self,
        context: &ItemContext,
        issue: NewIssue,
        repository: Option<String>,
        link: bool,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        let space = match self.chat_outside_repositories(context).await? {
            // The registered repository it names counts as its own.
            Some(chat) => match self.registered_named(repository.as_deref()).await? {
                Some(checkout) => LinkSpace::Chat { chat, checkout },
                None => {
                    anyhow::ensure!(!link, "{LINK_NEEDS_REPOSITORY}");
                    return self
                        .open_outside_issue(&chat, issue, repository, caller)
                        .await;
                }
            },
            None => self.context_space(context).await?,
        };
        let proposer = self.proposer(caller).await;
        anyhow::ensure!(!link || space.keeps_links(), "{LINK_NEEDS_REPOSITORY}");
        let checkout = self.context_checkout(&space.item_context()).await?;
        // The chat's gate keeps a link meanwhile from taking its issue's place.
        let _gate = self.gates.lock_prs([space.id()]).await;
        let own = self.checkout_forge(&checkout).await?;
        let destination = self.issue_destination(own, repository.as_deref()).await?;
        if link {
            anyhow::ensure!(
                self.issue_registration(space.id()).await?.is_none(),
                "the chat already has a linked issue; open the issue without --link"
            );
            anyhow::ensure!(
                !destination.elsewhere,
                "an issue in another repository cannot be linked; open it without --link"
            );
        }
        if let Some(proposer) = proposer {
            let settings = self.space_settings(&space).await?;
            if destination.opening(&settings)? == Opening::Propose {
                let required = destination.required(&settings);
                return (self.propose_issue_at(
                    &space,
                    &destination,
                    required,
                    issue,
                    link,
                    proposer,
                ))
                .await;
            }
        }
        let OpenOutcome::Opened(mut opened) =
            (self.open_checkout_issue(&checkout.id, issue, repository)).await?
        else {
            bail!("a chat opens issues without a proposal");
        };
        if space.keeps_links() {
            let (linked, warning) = self
                .keep_created(space.id(), ItemKind::Issue, &opened.item, link)
                .await?;
            opened.linked = linked;
            opened.warnings.extend(warning);
        }
        Ok(OpenOutcome::Opened(opened))
    }

    /// The chat without a repository the chat `context` names, if it is one.
    async fn chat_outside_repositories(
        &self,
        context: &ItemContext,
    ) -> Result<Option<crate::model::Chat>> {
        let ItemContext::Chat(selector) = context else {
            return Ok(None);
        };
        let chat = self.recorded_chat(selector).await?;
        Ok(chat.filter(|chat| chat.repository_id.is_none()))
    }

    /// Open the issue a chat without a repository opens in the repository
    /// the URL `repository` names, which is not registered, and so another
    /// repository under the global `[permissions] issue_other_repositories`.
    /// A proposal belongs to a registered repository, so a scoped `caller`
    /// that the setting would have propose one is refused.
    async fn open_outside_issue(
        &self,
        chat: &crate::model::Chat,
        issue: NewIssue,
        repository: Option<String>,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        let named = repository
            .context("a chat without a repository names where the issue opens; pass --repo")?;
        let forge = ForgeRepo::resolve(&named, &self.forge_hosts())
            .await
            .with_context(|| format!("{named} is not registered; name it by its URL"))?;
        let destination = IssueDestination {
            forge,
            elsewhere: true,
        };
        if self.proposer(caller).await.is_some() {
            let settings = self.chat_settings(chat).await?;
            anyhow::ensure!(
                destination.opening(&settings)? == Opening::Direct,
                "agents propose issues only in registered repositories from a chat without a \
                 repository, and [permissions] issue_other_repositories asks for a proposal; ask \
                 the user to open it"
            );
        }
        let Created { item, unfinished } = (destination.forge)
            .create_issue(&self.agent_account(), &issue)
            .await?;
        Ok(OpenOutcome::opened(Opened {
            item,
            created: true,
            linked: false,
            warnings: unfinished.into_iter().collect(),
        }))
    }

    /// Open the issue for the workspace and record it as opened there. A
    /// workspace process's issue is recorded for the user's approval instead
    /// when `[permissions] issue_open`, or for another repository
    /// `[permissions] issue_other_repositories`, says so.
    async fn open_workspace_issue(
        &self,
        selector: &str,
        issue: NewIssue,
        repository: Option<String>,
        link: bool,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        let proposer = self.proposer(caller).await;
        // Cleanup holds the workspace's gate, so it cannot remove the
        // workspace between opening the issue and linking it.
        let (workspace, _gate) = self.lock_pr_gate(selector).await?;
        self.verify_worktree(&workspace).await?;
        // Refuse before creating anything that could not then be recorded
        // or linked.
        anyhow::ensure!(
            workspace.state.accepts_links(),
            "workspace cannot accept an issue in its current state"
        );
        anyhow::ensure!(
            !link || workspace.links.issue.is_none(),
            "the workspace already has a linked issue; open the issue without --link"
        );
        let settings = self.workspace_settings(&workspace).await?;
        let (own, account) = self.workspace_forge_account(&workspace).await?;
        let destination = self.issue_destination(own, repository.as_deref()).await?;
        anyhow::ensure!(
            !link || !destination.elsewhere,
            "an issue in another repository cannot be linked; open it without --link"
        );
        let opening = match proposer {
            Some(_) => destination.opening(&settings)?,
            None => Opening::Direct,
        };
        let (forge, required) = (&destination.forge, destination.required(&settings));
        if let Some(proposer) = proposer.filter(|_| opening == Opening::Propose) {
            let space = LinkSpace::from(workspace.clone());
            return (self.propose_issue_at(&space, &destination, required, issue, link, proposer))
                .await;
        }
        conventions::require(forge, &account, ItemKind::Issue, required, &issue.labels).await?;
        let Created { item, unfinished } = forge.create_issue(&account, &issue).await?;
        let (linked, warning) = self
            .keep_created(&workspace.id, ItemKind::Issue, &item, link)
            .await?;
        let checked = destination
            .warnings(&account, &workspace.path, &item, &issue, required)
            .await;
        Ok(OpenOutcome::opened(Opened {
            item,
            created: true,
            linked,
            warnings: warning
                .into_iter()
                .chain(unfinished)
                .chain(checked)
                .collect(),
        }))
    }

    /// Record the issue for the user's approval instead of opening it in
    /// `destination`, refusing it first for what opening it would refuse:
    /// a label in `required` it leaves out, or a file the forge does not
    /// take.
    async fn propose_issue_at(
        &self,
        space: &LinkSpace,
        destination: &IssueDestination,
        required: &[String],
        issue: NewIssue,
        link: bool,
        proposer: Proposer,
    ) -> Result<OpenOutcome> {
        let (forge, account) = (&destination.forge, self.agent_account());
        conventions::require(forge, &account, ItemKind::Issue, required, &issue.labels).await?;
        forge.check_uploads(&account, &issue.attachments).await?;
        let repository = destination.repository();
        let proposed = self
            .propose_issue(space, issue, repository, link, proposer)
            .await?;
        Ok(OpenOutcome::Proposed(proposed))
    }

    /// Open the issue from the repository's registered checkout, under the
    /// repository's settings. Only unscoped callers open from a checkout, so
    /// nothing is proposed.
    async fn open_checkout_issue(
        &self,
        selector: &str,
        issue: NewIssue,
        repository: Option<String>,
    ) -> Result<OpenOutcome> {
        let checkout = self.repository(selector).await?;
        let settings = self.repository_settings(&checkout.id).await?;
        let (own, account) = self.checkout_forge_account(&checkout).await?;
        let destination = self.issue_destination(own, repository.as_deref()).await?;
        let (forge, required) = (&destination.forge, destination.required(&settings));
        conventions::require(forge, &account, ItemKind::Issue, required, &issue.labels).await?;
        let Created { item, unfinished } = forge.create_issue(&account, &issue).await?;
        let checked = destination
            .warnings(&account, &checkout.path, &item, &issue, required)
            .await;
        Ok(OpenOutcome::opened(Opened {
            item,
            created: true,
            linked: false,
            warnings: unfinished.into_iter().chain(checked).collect(),
        }))
    }

    /// The repository `named` names for a new issue, or the workspace's
    /// repository `own` without one.
    async fn issue_destination(
        &self,
        own: ForgeRepo,
        named: Option<&str>,
    ) -> Result<IssueDestination> {
        let forge = self.named_forge(own.clone(), named).await?;
        let elsewhere = forge != own;
        Ok(IssueDestination { forge, elsewhere })
    }
}

/// The repository a new issue opens in, and whether it is another than the
/// workspace's, whose settings and conventions then do not apply.
struct IssueDestination {
    forge: ForgeRepo,
    elsewhere: bool,
}

impl IssueDestination {
    /// How a scoped caller opens the issue here under `settings`, refusing
    /// another repository they deny.
    fn opening(&self, settings: &Effective) -> Result<Opening> {
        if !self.elsewhere {
            return Ok(settings.permissions.issue_open);
        }
        match settings.permissions.issue_other_repositories {
            OtherRepositories::Deny => bail!(
                "agents cannot open issues in {}: [permissions] issue_other_repositories is \"deny\"",
                self.forge.repository_url()
            ),
            OtherRepositories::Propose => Ok(Opening::Propose),
            OtherRepositories::Direct => Ok(Opening::Direct),
        }
    }

    /// The labels `settings` require of the new issue; none elsewhere.
    fn required<'a>(&self, settings: &'a Effective) -> &'a [String] {
        if self.elsewhere {
            &[]
        } else {
            &settings.issue.required_labels
        }
    }

    /// What the new `item`, opened from `worktree`, leaves out of the
    /// conventions: the workspace's conventions do not apply to another
    /// repository, and its item commands cannot change an issue there.
    async fn warnings(
        &self,
        account: &Account,
        worktree: &Path,
        item: &Item,
        issue: &NewIssue,
        required: &[String],
    ) -> Vec<String> {
        if self.elsewhere {
            return Vec::new();
        }
        let body = Some(issue.body.as_str());
        conventions::warnings(&self.forge, account, worktree, item, body, required).await
    }

    /// The URL a proposal records for another repository.
    fn repository(&self) -> Option<String> {
        self.elsewhere.then(|| self.forge.repository_url())
    }
}
