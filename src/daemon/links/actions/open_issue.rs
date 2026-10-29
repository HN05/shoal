//! Opening an issue for a workspace, in its repository or another one, or
//! recording it for the user's approval where the repository asks for that.
use anyhow::{Result, bail};

use crate::{
    config::{
        repo::{Opening, OtherRepositories},
        resolve::Effective,
    },
    daemon::{links::proposals::OpenOutcome, scope::Caller, workspace::Manager},
    forge::{
        ForgeRepo, conventions,
        items::{
            create::{Created, NewIssue},
            item::Opened,
            link::ItemKind,
        },
    },
};

impl Manager {
    /// Open an issue as the workspace's agent account in its repository, or
    /// the one `repository` names, and link it when `link` is set. A
    /// workspace process's issue is recorded for the user's approval instead
    /// when `[approvals] issue_open`, or for another repository
    /// `[issue] other_repositories`, says so.
    pub async fn open_issue(
        &self,
        selector: &str,
        issue: NewIssue,
        repository: Option<String>,
        link: bool,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        anyhow::ensure!(!issue.title.trim().is_empty(), "the issue needs a title");
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
        // The workspace's conventions do not apply to another repository,
        // and its item commands cannot change an issue there.
        let checked = if destination.elsewhere {
            Vec::new()
        } else {
            let body = Some(issue.body.as_str());
            conventions::warnings(forge, &account, &workspace.path, &item, body, required).await
        };
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

    /// The URL a proposal records for another repository.
    fn repository(&self) -> Option<String> {
        self.elsewhere.then(|| self.forge.repository_url())
    }
}
