//! Opening an issue for a workspace or from a registered checkout, in its
//! repository or another one, or recording it for the user's approval where
//! the repository asks for that.
use std::path::Path;

use anyhow::{Result, bail};

use crate::{
    config::{
        repo::{Opening, OtherRepositories},
        resolve::Effective,
    },
    daemon::{links::proposals::OpenOutcome, scope::Caller, workspace::Manager},
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

/// Why `--link` is refused outside a workspace.
pub(crate) const LINK_NEEDS_WORKSPACE: &str =
    "--link needs a workspace to assign the issue to; run this in one or pass --workspace";

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
            ItemContext::Checkout(selector) => {
                anyhow::ensure!(!link, "{LINK_NEEDS_WORKSPACE}");
                self.open_checkout_issue(selector, issue, repository).await
            }
        }
    }

    /// Open the issue for the workspace and record it as opened there. A
    /// workspace process's issue is recorded for the user's approval instead
    /// when `[approvals] issue_open`, or for another repository
    /// `[issue] other_repositories`, says so.
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
        conventions::require(forge, &account, ItemKind::Issue, required, &issue.labels).await?;
        if let Some(proposer) = proposer.filter(|_| opening == Opening::Propose) {
            forge.check_uploads(&account, &issue.attachments).await?;
            let repository = destination.repository();
            let proposed = self
                .propose_issue(&workspace, issue, repository, link, proposer)
                .await?;
            return Ok(OpenOutcome::Proposed(proposed));
        }
        let Created { item, unfinished } = forge.create_issue(&account, &issue).await?;
        let (linked, warning) = self
            .keep_created(&workspace, ItemKind::Issue, &item, link)
            .await?;
        let checked = destination
            .warnings(&account, &workspace.path, &item, &issue, required)
            .await;
        Ok(OpenOutcome::Opened(Opened {
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
        Ok(OpenOutcome::Opened(Opened {
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
        let Some(named) = named else {
            return Ok(IssueDestination {
                forge: own,
                elsewhere: false,
            });
        };
        let forge = own.resolve_named(named, &self.forge_hosts()).await?;
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
            return Ok(settings.approvals.issue_open);
        }
        match settings.issue.other_repositories {
            OtherRepositories::Deny => bail!(
                "agents cannot open issues in {}: [issue] other_repositories is \"deny\"",
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
