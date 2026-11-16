//! Where a launch of a workspace starts: the registered repository it, or
//! the issue or link it names, belongs to, else the one of the checkout or
//! worktree the start was made in; and what the positional argument starts
//! on, an issue, or a PR's or a branch link's branch, which fills the
//! creation.
use anyhow::{Context as _, Result, ensure};
use shoal_protocol::launches::{IssueChoice, Launch, StartTarget};

use super::ManagerReads;
use crate::{
    daemon::workspace::Manager,
    forge::items::reference::{ItemInput, ItemRef},
    launches::workspace,
    model::Repository,
    start_plan::Creation,
};

/// The repository and issue a launch of a workspace starts on.
pub(super) struct Target {
    pub repo: Repository,
    pub issue: Option<ItemInput>,
    /// Its agent defaults to `agent.default`, as a start on a positional
    /// issue's does, rather than to none.
    pub issue_default: bool,
}

impl Manager {
    /// The target of `launch`, whose positional argument classifies as a
    /// repository, an issue, or a PR or branch link, which completes
    /// `creation`. A copy goes in the copied workspace's repository unless
    /// the launch names one.
    pub(super) async fn workspace_target(
        &self,
        launch: &Launch,
        creation: &mut Creation,
        copied: Option<&Repository>,
        reads: &ManagerReads<'_>,
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
            return self.target(launch, named, issue, false, reads).await;
        };
        let names_branch = creation.branch.is_some() || issue.is_some();
        if let StartTarget::Repository(named) = positional {
            ensure!(
                launch.repository.is_none(),
                "a positional repository cannot be combined with --repo"
            );
            return self.target(launch, Some(named), issue, false, reads).await;
        }
        ensure!(
            !names_branch,
            "an item or branch link cannot be combined with --branch or --issue"
        );
        if let StartTarget::Issue(issue) = positional {
            let named = launch.repository.clone();
            return self.target(launch, named, Some(issue), true, reads).await;
        }
        let repo = self.linked_repository(launch, &positional, reads).await?;
        workspace::ensure_link_in(&repo, positional.linked_repository()).await?;
        match positional {
            StartTarget::Pull(pull) => self.pull_target(&repo, pull, creation, reads).await?,
            StartTarget::Branch { branch, .. } => {
                workspace::branch_creation(&branch, creation).await?
            }
            StartTarget::Repository(_) | StartTarget::Number(_) | StartTarget::Issue(_) => {
                unreachable!("returned above")
            }
        }
        Ok(Target {
            repo,
            issue: None,
            issue_default: false,
        })
    }

    /// The target in the repository `named` names, else that of `issue`'s
    /// URL, else that of the checkout or worktree the start was made in.
    async fn target(
        &self,
        launch: &Launch,
        named: Option<String>,
        issue: Option<ItemInput>,
        issue_default: bool,
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
                self.current_repository(launch, &repos, reads, hint).await?
            }
        };
        Ok(Target {
            repo,
            issue,
            issue_default,
        })
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
        let id = (self
            .directory_repository(launch, repos, reads.caller)
            .await?)
            .with_context(|| format!("no current registered repository; {hint}"))?;
        self.repository(id).await
    }

    /// Complete `creation` for the PR `pull` names in `repo`.
    async fn pull_target(
        &self,
        repo: &Repository,
        pull: ItemRef,
        creation: &mut Creation,
        reads: &ManagerReads<'_>,
    ) -> Result<()> {
        let context = self.launch_read_context(&repo.id, reads.caller).await?;
        let pull = self
            .pull_request(&context, &pull.into(), reads.caller)
            .await?;
        workspace::pull_creation(reads, repo, pull, creation).await
    }
}

/// The only registered repository among `repos` whose origin the issue
/// `item` belongs to.
async fn item_repository<'a>(repos: &'a [Repository], item: &ItemRef) -> Result<&'a Repository> {
    item.of_kind(crate::forge::items::link::ItemKind::Issue.into())?;
    let retry = "shoal start <repository> --issue <url>";
    crate::repository::with_remote(repos, item.repository().clone(), retry).await
}
