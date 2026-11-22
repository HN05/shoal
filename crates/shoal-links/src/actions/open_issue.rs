//! Opening an issue for a space, or from a registered
//! checkout, in its repository or another one, or recording it for the
//! user's approval where the repository asks for that.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result, bail};
use shoal_core::config::Effective;
use shoal_daemon_base::callers::Caller;
use shoal_forge::{
    conventions,
    items::{
        attach::AttachRequests,
        create::{CreateRequests, Created},
    },
    transport::account::Account,
};
use shoal_forge_types::{
    ForgeRepo,
    items::{
        create::NewIssue,
        item::{Item, Opened},
        link::ItemKind,
    },
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::{
    config::permissions::{Opening, OtherRepositories, Permissions},
    links::OpenOutcome,
    model::{Assigner, Space},
    protocol::{ItemContext, RepoTarget},
};

use crate::{
    Links,
    daemon::Daemon,
    forges,
    issue::assignees::AssignerOf,
    proposals::{OpenOutcomeExt, Proposer, proposer},
    space::LinkSpace,
};

/// Why `--link` is refused outside a workspace or chat.
const LINK_NEEDS_SPACE: &str =
    "--link needs a workspace or chat to assign the issue to; run this in one or pass --space";
/// Why `--link` is refused in a chat without a repository.
const LINK_NEEDS_REPOSITORY: &str =
    "a chat without a repository links no issues; open the issue without --link";

impl Links {
    /// Open an issue as the agent account in the `target` repository, and
    /// assign it to the space when `link` is set.
    pub async fn open_issue(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        issue: NewIssue,
        link: bool,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        anyhow::ensure!(!issue.title.trim().is_empty(), "the issue needs a title");
        let repository = target.repository.clone();
        let context = match &target.context {
            Some(ItemContext::Checkout(id)) => {
                anyhow::ensure!(!link, "{LINK_NEEDS_SPACE}");
                let account = self.user_account();
                return (self.open_checkout_issue(daemon, id, issue, repository, account)).await;
            }
            Some(context) => context,
            None => {
                anyhow::ensure!(!link, "{LINK_NEEDS_SPACE}");
                return self
                    .open_outside_issue(daemon, issue, repository, caller)
                    .await;
            }
        };
        match self
            .issue_space(daemon, context, repository.as_deref())
            .await?
        {
            Some(space) => {
                (self.open_space_issue(daemon, space, issue, repository, link, caller)).await
            }
            None => {
                anyhow::ensure!(!link, "{LINK_NEEDS_REPOSITORY}");
                self.open_outside_issue(daemon, issue, repository, caller)
                    .await
            }
        }
    }

    /// The space `context` names, acting in the repository its issue opens
    /// in: its own, or for a chat without one, the registered repository
    /// `named` names, which counts as its own. None for such a chat naming
    /// no registered repository.
    async fn issue_space(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        context: &ItemContext,
        named: Option<&str>,
    ) -> Result<Option<LinkSpace>> {
        match self.outside_repositories(daemon, context).await? {
            Some(chat) => Ok((daemon.registered_named(named).await?)
                .map(|checkout| LinkSpace::acting_in(chat, checkout))),
            None => self.context_space(daemon, context).await.map(Some),
        }
    }

    /// The space without a repository, a chat, that `context` names acting
    /// in no registered repository, if it is one.
    async fn outside_repositories(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        context: &ItemContext,
    ) -> Result<Option<Space>> {
        let ItemContext::Space {
            space,
            repository: None,
        } = context
        else {
            return Ok(None);
        };
        let space = daemon.space(space).await.ok();
        Ok(space.filter(|space| space.repository_id.is_none()))
    }

    /// Open the issue a request from no repository, or a chat without one,
    /// opens in the repository the URL `repository` names, which is not
    /// registered, and so another repository under a scoped `caller`'s
    /// `issue_other_repositories` permission in the global config, as its
    /// agent account, and as the user's own logins for the user.
    /// A proposal belongs to a registered repository, so a scoped `caller`
    /// that the setting would have propose one is refused.
    async fn open_outside_issue(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        issue: NewIssue,
        repository: Option<String>,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        let named = repository.context("name the repository the issue opens in with --repo")?;
        let forge = (daemon.resolve_forge(&named).await)
            .with_context(|| format!("{named} is not registered; name it by its URL"))?;
        let destination = IssueDestination {
            forge,
            elsewhere: true,
        };
        if let Some(caller) = caller {
            // No registered repository's config applies, so the global one does.
            let permissions = daemon.caller_permissions(caller, None).await?;
            anyhow::ensure!(
                destination.opening(&permissions)? == Opening::Direct,
                "agents propose issues only in registered repositories from a chat without a \
                 repository, and {}; ask the user to open it",
                permissions
                    .issue_other_repositories
                    .cited("issue_other_repositories")
            );
        }
        let account = match caller {
            Some(_) => self.agent_account(),
            None => self.user_account(),
        };
        let Created { item, unfinished } =
            (destination.forge).create_issue(&account, &issue).await?;
        Ok(OpenOutcome::opened(Opened {
            item,
            created: true,
            linked: false,
            warnings: unfinished.into_iter().collect(),
        }))
    }

    /// Open the issue for the space and record it as opened there, or
    /// assign it with `link`; a chat without a repository records nothing.
    /// A scoped `caller`'s issue is recorded for approval instead when its
    /// `issue_open` permission, or for another repository its
    /// `issue_other_repositories` permission, says so.
    async fn open_space_issue(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        space: LinkSpace,
        issue: NewIssue,
        repository: Option<String>,
        link: bool,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        anyhow::ensure!(!link || space.keeps_links(), "{LINK_NEEDS_REPOSITORY}");
        let proposer = proposer(daemon, caller).await;
        // Cleanup holds the space's gate, so it cannot remove the space
        // between opening the issue and linking it, nor a link take its
        // issue's place meanwhile.
        let _gate = self.gates.lock_prs([&space.id]).await;
        let directory = self.issue_directory(daemon, &space).await?;
        let settings = daemon.acting_settings(&space).await?;
        let own = forges::origin(daemon, &directory).await?;
        let destination = self
            .issue_destination(daemon, own, repository.as_deref())
            .await?;
        anyhow::ensure!(
            !link || !destination.elsewhere,
            "an issue in another repository cannot be linked; open it without --link"
        );
        let opening = match caller {
            Some(caller) => {
                let permissions = daemon.caller_permissions(caller, Some(space.repository_id()));
                destination.opening(&permissions.await?)?
            }
            None => Opening::Direct,
        };
        let required = destination.required(&settings);
        if let Some(proposer) = proposer.filter(|_| opening != Opening::Direct) {
            return (self.propose_issue_at(
                daemon,
                &space,
                &destination,
                required,
                issue,
                link,
                proposer,
            ))
            .await;
        }
        let account = self.agent_account();
        let created = destination.create(&account, &directory, &issue, required);
        let (item, warnings) = created.await?;
        (self.keep_opened_issue(daemon, &space, item, warnings, link, caller)).await
    }

    /// The issue the space just opened, recorded as opened there, or
    /// assigned with `link`, after `warnings` and what linking it warns of;
    /// a chat without a repository records nothing.
    async fn keep_opened_issue(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        space: &LinkSpace,
        item: Item,
        warnings: Vec<String>,
        link: bool,
        caller: Option<&Caller>,
    ) -> Result<OpenOutcome> {
        let (linked, kept) = match space.keeps_links() {
            true => {
                let assigner = Assigner::of(caller);
                (self.keep_created(daemon, &space.id, ItemKind::Issue, &item, link, assigner))
                    .await?
            }
            false => (false, Vec::new()),
        };
        Ok(OpenOutcome::opened(Opened {
            item,
            created: true,
            linked,
            warnings: kept.into_iter().chain(warnings).collect(),
        }))
    }

    /// The directory the space opens issues from, whose `origin` names its
    /// repository and whose files give its conventions: a workspace's
    /// worktree, verified and able to record what it opens, or the
    /// registered checkout of the repository a chat acts in. A workspace is
    /// read again, under the gate the caller holds.
    async fn issue_directory(&self, daemon: &dyn Daemon, space: &LinkSpace) -> Result<PathBuf> {
        if space.worktree().is_none() {
            return Ok(daemon.repository(space.repository_id()).await?.path);
        }
        let workspace = daemon.workspace_by_id(&space.id).await?;
        daemon.verify_worktree(&workspace).await?;
        // Refuse before creating anything that could not then be recorded
        // or linked.
        anyhow::ensure!(
            workspace.state.accepts_links(),
            "workspace cannot accept an issue in its current state"
        );
        Ok(workspace.path.clone())
    }

    /// Record the issue for the user's approval instead of opening it in
    /// `destination`, refusing it first for what opening it would refuse:
    /// a label in `required` it leaves out, or a file the forge does not
    /// take.
    #[allow(
        clippy::too_many_arguments,
        reason = "the issue and where it opens, beside the daemon and its proposer"
    )]
    async fn propose_issue_at(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
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
            .propose_issue(daemon, space, issue, repository, link, proposer)
            .await?;
        Ok(OpenOutcome::Proposed(proposed))
    }

    /// Open the issue from the repository's registered checkout, under the
    /// repository's settings, as `account`. Only unscoped callers open from
    /// a checkout, so nothing is proposed.
    async fn open_checkout_issue(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        id: &RepositoryId,
        issue: NewIssue,
        repository: Option<String>,
        account: Account,
    ) -> Result<OpenOutcome> {
        let checkout = daemon.repository(id).await?;
        let settings = daemon.repository_settings(&checkout.id).await?;
        let own = forges::of_checkout(daemon, &checkout).await?;
        let destination = self
            .issue_destination(daemon, own, repository.as_deref())
            .await?;
        let required = destination.required(&settings);
        let (item, warnings) = destination
            .create(&account, &checkout.path, &issue, required)
            .await?;
        Ok(OpenOutcome::opened(Opened {
            item,
            created: true,
            linked: false,
            warnings,
        }))
    }

    /// The repository `named` names for a new issue, or the workspace's
    /// repository `own` without one.
    async fn issue_destination(
        self: &Arc<Self>,
        daemon: &dyn Daemon,
        own: ForgeRepo,
        named: Option<&str>,
    ) -> Result<IssueDestination> {
        let forge = forges::named(daemon, own.clone(), named).await?;
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
    /// How a scoped caller with `permissions` opens the issue here,
    /// refusing another repository they deny.
    fn opening(&self, permissions: &Permissions) -> Result<Opening> {
        if !self.elsewhere {
            return Ok(permissions.issue_open.value);
        }
        let other = permissions.issue_other_repositories;
        match other.value {
            OtherRepositories::Deny => bail!(
                "agents cannot open issues in {}: {}",
                self.forge.repository_url(),
                other.cited("issue_other_repositories")
            ),
            OtherRepositories::Propose => Ok(Opening::Propose),
            OtherRepositories::Gate => Ok(Opening::Gate),
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

    /// Create `issue` here as `account`, from `directory`, refusing it
    /// without a label in `required`, with the warnings of what it leaves
    /// unfinished or out of the conventions.
    async fn create(
        &self,
        account: &Account,
        directory: &Path,
        issue: &NewIssue,
        required: &[String],
    ) -> Result<(Item, Vec<String>)> {
        let forge = &self.forge;
        conventions::require(forge, account, ItemKind::Issue, required, &issue.labels).await?;
        let Created { item, unfinished } = forge.create_issue(account, issue).await?;
        let checked = self
            .warnings(account, directory, &item, issue, required)
            .await;
        let warnings = unfinished.into_iter().chain(checked).collect();
        Ok((item, warnings))
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
