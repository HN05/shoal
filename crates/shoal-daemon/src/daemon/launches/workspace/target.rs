//! Where a launch of a workspace starts: the registered repository it, or
//! the issue or link it names, belongs to, else the one of the checkout or
//! worktree the start was made in; and what the positional argument starts
//! on, an issue, or a PR's or a branch link's branch, which fills the
//! creation.
use anyhow::{Context as _, Result, ensure};
use shoal_protocol::launches::{IssueChoice, Launch, StartTarget};

use super::ManagerReads;
use crate::{
    daemon::manager::Manager,
    forge::{
        PullRequest,
        items::reference::{ItemInput, ItemRef},
    },
    launches::workspace,
    model::Repository,
    start_plan::Creation,
};

/// The repository and issue a launch of a workspace starts on.
pub(super) struct Target {
    pub repo: Repository,
    pub issue: Option<ItemInput>,
}

impl Manager {
    /// The target of `launch`, whose positional argument classifies as a
    /// repository, an issue, or a PR or branch link, which completes
    /// `creation`, fetching a PR's branches where it `fetches`. A copy goes
    /// in the copied workspace's repository unless the launch names one.
    pub(super) async fn workspace_target(
        &self,
        launch: &Launch,
        creation: &mut Creation,
        copied: Option<&Repository>,
        (reads, fetches): (&ManagerReads<'_>, bool),
    ) -> Result<Target> {
        let issue = match &launch.issue {
            Some(IssueChoice::Pick) => {
                anyhow::bail!("--issue needs a number or URL outside plan mode")
            }
            Some(IssueChoice::Named(issue)) => Some(issue.clone()),
            None => None,
        };
        let Some(positional) = crate::launches::decided_target(launch)? else {
            let named = launch
                .repository
                .clone()
                .or(copied.map(|repo| repo.id.to_string()));
            return self.target(launch, named, issue, reads).await;
        };
        let names_branch = creation.branch.is_some() || issue.is_some();
        if let StartTarget::Repository(named) = positional {
            ensure!(
                launch.repository.is_none(),
                "a positional repository cannot be combined with --repo"
            );
            return self.target(launch, Some(named), issue, reads).await;
        }
        ensure!(
            !names_branch,
            "an item or branch link cannot be combined with --branch or --issue"
        );
        if let StartTarget::Issue(issue) = positional {
            let named = launch.repository.clone();
            return self.target(launch, named, Some(issue), reads).await;
        }
        let repo = self.linked_repository(launch, &positional, reads).await?;
        workspace::ensure_link_in(&repo, positional.linked_repository()).await?;
        match positional {
            StartTarget::Pull(pull) => {
                (self.pull_target(&repo, pull, creation, (reads, fetches))).await?
            }
            StartTarget::Branch { branch, .. } => {
                super::branch::branch_creation(&branch, creation).await?
            }
            StartTarget::Repository(_) | StartTarget::Number(_) | StartTarget::Issue(_) => {
                unreachable!("returned above")
            }
        }
        Ok(Target { repo, issue: None })
    }

    /// The target in the repository `named` names, else that of `issue`'s
    /// URL, else that of the space the launch goes below, which a child
    /// shares, else that of the checkout or worktree the start was made in.
    async fn target(
        &self,
        launch: &Launch,
        named: Option<String>,
        issue: Option<ItemInput>,
        reads: &ManagerReads<'_>,
    ) -> Result<Target> {
        let repos = self.repositories().await?;
        let repo = match (named, &issue) {
            (Some(named), _) => {
                let selector = crate::repository::named_in(named, launch.directory.as_deref())?;
                crate::repository::select(&repos, selector).await?.clone()
            }
            (None, Some(ItemInput::Url(item))) => item_repository(&repos, item).await?.clone(),
            (None, issue) => {
                let hint = match issue {
                    Some(_) => "pass --repo <repository> or a forge URL",
                    None => "pass <repository>",
                };
                match crate::launches::shape::parent_repository(reads, launch).await? {
                    Some(repository) => self.repository(repository).await?,
                    None => self.current_repository(launch, &repos, reads, hint).await?,
                }
            }
        };
        Ok(Target { repo, issue })
    }

    /// The registered repository a PR or branch link of `launch` belongs to:
    /// the one `--repo` names, else its remote's, else the current one.
    async fn linked_repository(
        &self,
        launch: &Launch,
        positional: &StartTarget,
        reads: &ManagerReads<'_>,
    ) -> Result<Repository> {
        let repos = self.repositories().await?;
        if let Some(named) = &launch.repository {
            let directory = launch.directory.as_deref();
            let selector = crate::repository::named_in(named.clone(), directory)?;
            return Ok(crate::repository::select(&repos, selector).await?.clone());
        }
        match positional.linked_repository() {
            Some(linked) => {
                let retry = "shoal start <link> --repo <repository>";
                let repo = crate::repository::with_remote(&repos, linked.clone(), retry).await?;
                Ok(repo.clone())
            }
            None => {
                let hint = "pass --repo <repository> or a forge URL";
                self.current_repository(launch, &repos, reads, hint).await
            }
        }
    }

    /// The repository of the checkout or worktree the start of `launch` was
    /// made in, refused with `hint` when there is none.
    async fn current_repository(
        &self,
        launch: &Launch,
        repos: &[Repository],
        reads: &ManagerReads<'_>,
        hint: &str,
    ) -> Result<Repository> {
        let id = (crate::launches::shape::directory_repository(reads, launch, repos).await?)
            .with_context(|| format!("no current registered repository; {hint}"))?;
        self.repository(id).await
    }

    /// Complete `creation` for the PR `pull` names in `repo`, fetching its
    /// branches where it `fetches`.
    async fn pull_target(
        &self,
        repo: &Repository,
        pull: ItemRef,
        creation: &mut Creation,
        (reads, fetches): (&ManagerReads<'_>, bool),
    ) -> Result<()> {
        let context = self.launch_read_context(&repo.id, reads.caller).await?;
        let pull = self
            .pull_request(&context, &pull.into(), reads.caller)
            .await?;
        match fetches {
            true => workspace::pull_creation(reads, repo, pull, creation).await,
            false => pull_settled(reads, repo, &pull, creation).await,
        }
    }
}

/// Settle `creation` on `pull` as [`workspace::pull_creation`] does, short
/// of fetching its branches: a fork's head refuses what only a branch
/// takes, and another PR opens its head branch, in the workspace that owns
/// it if any.
async fn pull_settled(
    reads: &ManagerReads<'_>,
    repo: &Repository,
    pull: &PullRequest,
    creation: &mut Creation,
) -> Result<()> {
    match pull.fork {
        true => workspace::ensure_fork_opens(pull, creation),
        false => (workspace::pull_head_settled(reads, repo, pull, creation).await).map(drop),
    }
}

/// The only registered repository among `repos` whose origin the issue
/// `item` belongs to.
async fn item_repository<'a>(repos: &'a [Repository], item: &ItemRef) -> Result<&'a Repository> {
    item.of_kind(crate::forge::items::link::ItemKind::Issue.into())?;
    let retry = "shoal start <repository> --issue <url>";
    crate::repository::with_remote(repos, item.repository().clone(), retry).await
}
