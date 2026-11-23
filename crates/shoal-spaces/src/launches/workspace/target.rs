//! Where a launch of a workspace starts: the registered repository it, or
//! the issue or link it names, belongs to, else the one of the checkout or
//! worktree the start was made in; and what the positional argument starts
//! on, an issue, or a PR's or a branch link's branch, which fills the
//! creation.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, ensure};
use shoal_protocol::launches::{ItemChoice, Launch, StartTarget};

use crate::{
    forge::{
        PullRequest,
        items::reference::{ItemInput, ItemRef},
    },
    model::{Repository, Workspace},
    scope::Caller,
    start_plan::Creation,
};
use shoal_core::launches::{LaunchState, workspace};

/// The repository and issue a launch of a workspace starts on.
pub(super) struct Target {
    pub repo: Repository,
    pub issue: Option<ItemInput>,
}

impl Spaces {
    /// The target of `launch`, whose positional argument classifies as a
    /// repository, an issue, or a PR or branch link, which completes
    /// `creation`, fetching a PR's branches where it `fetches`, from what
    /// `state` holds for `caller`. A copy goes in the copied workspace's
    /// repository unless the launch names one.
    pub(super) async fn workspace_target(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        creation: &mut Creation,
        copied: Option<&Repository>,
        (state, fetches, caller): (&LaunchState, bool, Option<&Caller>),
    ) -> Result<Target> {
        let issue = match &launch.issue {
            Some(ItemChoice::Pick) => {
                anyhow::bail!("--issue needs a number or URL; a terminal picks one without it")
            }
            Some(ItemChoice::Named(issue)) => Some(issue.clone()),
            None => None,
        };
        if let Some(pull) = &launch.pr {
            let reading = (state, fetches, caller);
            return self
                .flagged_pull_target(daemon, launch, pull, creation, reading)
                .await;
        }
        let Some(positional) = shoal_core::launches::decided_target(launch)? else {
            let named = launch
                .repository
                .clone()
                .or(copied.map(|repo| repo.id.to_string()));
            return self.target(daemon, launch, named, issue, state).await;
        };
        let names_branch = creation.branch.is_some() || issue.is_some();
        if let StartTarget::Repository(named) = positional {
            ensure!(
                launch.repository.is_none(),
                "a positional repository cannot be combined with --repo"
            );
            return self.target(daemon, launch, Some(named), issue, state).await;
        }
        ensure!(
            !names_branch,
            "an item or branch link cannot be combined with --branch or --issue"
        );
        if let StartTarget::Issue(issue) = positional {
            let named = launch.repository.clone();
            return self.target(daemon, launch, named, Some(issue), state).await;
        }
        let repo = self
            .linked_repository(daemon, launch, &positional, state)
            .await?;
        workspace::ensure_link_in(&repo, positional.linked_repository()).await?;
        match positional {
            StartTarget::Pull(pull) => {
                let reading = (state.workspaces.as_slice(), fetches, caller);
                (self.pull_target(daemon, &repo, pull.into(), creation, reading)).await?
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

    /// The target of `launch` on the PR `--pr` names, which completes
    /// `creation` as a positional PR link does, refusing what that link
    /// refuses beside it.
    async fn flagged_pull_target(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        pull: &ItemChoice,
        creation: &mut Creation,
        (state, fetches, caller): (&LaunchState, bool, Option<&Caller>),
    ) -> Result<Target> {
        let Some(pull) = pull.named() else {
            anyhow::bail!("--pr needs a number or URL; a terminal picks one without it");
        };
        ensure!(
            creation.branch.is_none() && launch.issue.is_none(),
            "--pr cannot be combined with --branch or --issue"
        );
        let repo = self
            .flagged_pull_repository(daemon, launch, pull, state)
            .await?;
        let reading = (state.workspaces.as_slice(), fetches, caller);
        (self.pull_target(daemon, &repo, pull.clone(), creation, reading)).await?;
        Ok(Target { repo, issue: None })
    }

    /// The registered repository of the PR `pull` that `--pr` names, among
    /// those `state` holds: the one `launch` names, else that of the PR's
    /// URL, else as for an issue number; a URL must be a PR's in it.
    async fn flagged_pull_repository(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        pull: &ItemInput,
        state: &LaunchState,
    ) -> Result<Repository> {
        let named = match shoal_core::launches::decided_target(launch)? {
            Some(StartTarget::Repository(named)) => {
                ensure!(
                    launch.repository.is_none(),
                    "a positional repository cannot be combined with --repo"
                );
                Some(named)
            }
            Some(_) => anyhow::bail!("--pr cannot be combined with an item or branch link"),
            None => launch.repository.clone(),
        };
        let repo = match (named, pull) {
            (None, ItemInput::Url(link)) => {
                let positional = StartTarget::Pull(link.clone());
                self.linked_repository(daemon, launch, &positional, state)
                    .await?
            }
            (named, _) => self.target(daemon, launch, named, None, state).await?.repo,
        };
        if let ItemInput::Url(link) = pull {
            link.of_kind(crate::forge::items::link::ItemKind::Pr.into())?;
            workspace::ensure_link_in(&repo, Some(link.repository())).await?;
        }
        Ok(repo)
    }

    /// The target in the repository `named` names, else that of `issue`'s
    /// URL, else that of the space the launch goes below, which a child
    /// shares, else that of the checkout or worktree the start was made in,
    /// as `state` holds them.
    async fn target(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        named: Option<String>,
        issue: Option<ItemInput>,
        state: &LaunchState,
    ) -> Result<Target> {
        let repos = &state.repositories;
        let repo = match (named, &issue) {
            (Some(named), _) => {
                let selector = crate::repository::named_in(named, launch.directory.as_deref())?;
                crate::repository::select(repos, selector).await?.clone()
            }
            (None, Some(ItemInput::Url(item))) => item_repository(repos, item).await?.clone(),
            (None, issue) => {
                let hint = match issue {
                    Some(_) => "pass --repo <repository> or a forge URL",
                    None => "pass <repository>",
                };
                match state.parent.clone().here()? {
                    Some(repository) => {
                        daemon
                            .repository(crate::repository::Selector::from(repository))
                            .await?
                    }
                    None => self.current_repository(daemon, launch, state, hint).await?,
                }
            }
        };
        Ok(Target { repo, issue })
    }

    /// The registered repository a PR or branch link of `launch` belongs to,
    /// among those `state` holds: the one `--repo` names, else its
    /// remote's, else the current one.
    async fn linked_repository(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        positional: &StartTarget,
        state: &LaunchState,
    ) -> Result<Repository> {
        let repos = &state.repositories;
        if let Some(named) = &launch.repository {
            let directory = launch.directory.as_deref();
            let selector = crate::repository::named_in(named.clone(), directory)?;
            return Ok(crate::repository::select(repos, selector).await?.clone());
        }
        match positional.linked_repository() {
            Some(linked) => {
                let retry = "shoal start <link> --repo <repository>";
                let repo = crate::repository::with_remote(repos, linked.clone(), retry).await?;
                Ok(repo.clone())
            }
            None => {
                let hint = "pass --repo <repository> or a forge URL";
                self.current_repository(daemon, launch, state, hint).await
            }
        }
    }

    /// The repository of the checkout or worktree the start of `launch` was
    /// made in, as `state` holds them, refused with `hint` when there is
    /// none.
    async fn current_repository(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        state: &LaunchState,
        hint: &str,
    ) -> Result<Repository> {
        let (repos, workspaces) = (&state.repositories, &state.workspaces);
        let id = (shoal_core::launches::shape::directory_repository(launch, repos, workspaces)
            .await?)
            .with_context(|| format!("no current registered repository; {hint}"))?;
        daemon
            .repository(crate::repository::Selector::from(id))
            .await
    }

    /// Complete `creation` for the PR `pull` names in `repo`, which
    /// `caller` reads, among the `workspaces` it sees, fetching its branches
    /// where it `fetches`.
    async fn pull_target(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        pull: ItemInput,
        creation: &mut Creation,
        (workspaces, fetches, caller): (&[Workspace], bool, Option<&Caller>),
    ) -> Result<()> {
        let context = self.launch_read_context(&repo.id, caller).await?;
        let pull = daemon.pull_request(&context, &pull, caller).await?;
        match fetches {
            true => workspace::pull_creation(workspaces, repo, pull, creation).await,
            false => pull_settled(workspaces, repo, &pull, creation),
        }
    }
}

/// Settle `creation` on `pull` as [`workspace::pull_creation`] does, short
/// of fetching its branches: a fork's head refuses what only a branch
/// takes, and another PR opens its head branch, in the one of `workspaces`
/// that owns it if any.
fn pull_settled(
    workspaces: &[Workspace],
    repo: &Repository,
    pull: &PullRequest,
    creation: &mut Creation,
) -> Result<()> {
    if pull.fork {
        return workspace::ensure_fork_opens(pull, creation);
    }
    workspace::pull_head_settled(workspaces, repo, pull, creation);
    Ok(())
}

/// The only registered repository among `repos` whose origin the issue
/// `item` belongs to.
async fn item_repository<'a>(repos: &'a [Repository], item: &ItemRef) -> Result<&'a Repository> {
    item.of_kind(crate::forge::items::link::ItemKind::Issue.into())?;
    let retry = "shoal start <repository> --issue <url>";
    crate::repository::with_remote(repos, item.repository().clone(), retry).await
}
