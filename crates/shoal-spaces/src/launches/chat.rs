//! Where a launch's new chat goes, and the issue it starts on, which a chat
//! takes only as the launch names it.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, ensure};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::{ItemChoice, Launch, Setup, SpaceChoice};

use super::{
    shape::Shape,
    space::{PlacedHome, Placement},
};
use crate::{
    forge::items::{link::ItemKind, reference::ItemInput},
    {
        scope::Caller,
        starts::{Home, StartIssue},
    },
};
use shoal_core::launches;

impl Spaces {
    pub(super) async fn chat_placement(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        shape: &Shape,
        caller: Option<&Caller>,
    ) -> Result<Placement> {
        let (home, issue) = self
            .settle_chat(daemon, launch, shape, false, caller)
            .await?;
        Ok(Placement {
            home: PlacedHome::Chat(home),
            issue,
            blockers: Vec::new(),
        })
    }

    /// Reads the issue it starts on. One a picker has yet to choose stays
    /// open where it `picks`.
    async fn settle_chat(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        shape: &Shape,
        picks: bool,
        caller: Option<&Caller>,
    ) -> Result<(Home<'static>, Option<StartIssue>)> {
        ensure_chat_takes(launch, shape)?;
        let in_workspace = launch.in_workspace.is_some();
        let (named, issue) = launches::chat_target(
            launch.target.clone(),
            launch.repository.clone(),
            launch.issue.clone(),
            in_workspace,
        )?;
        let issue = named_issue(issue, picks)?;
        let home =
            (self.launch_chat_home(daemon, launch, shape, (named, issue.as_ref()), caller)).await?;
        // A named parent is never the workspace the chat runs in. One it
        // leaves out is the caller's own space, wherever the chat runs.
        if let Home::Worktree(workspace) = &home {
            (self.ensure_parent_not_host(daemon, Some(workspace), launch.parent.as_ref())).await?;
        }
        let issue = match issue {
            Some(issue) => {
                let repository = (self.home_repository(daemon, &home).await?).context(
                    "a chat without a repository starts on no issue; name its repository",
                )?;
                // A chat its launch names needs no title from its issue.
                let by_url = launch.name.is_some();
                let read = self.launch_issue(daemon, &issue, (&repository, by_url), caller);
                Some(read.await?)
            }
            None => None,
        };
        Ok((home, issue))
    }

    /// In the worktree of the workspace it names, without a repository for a
    /// `shape` that has none, else in the checkout of the repository it
    /// `names`, or that of the issue it starts on or of where the start was
    /// made.
    async fn launch_chat_home(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        shape: &Shape,
        (named, issue): (Option<String>, Option<&ItemInput>),
        caller: Option<&Caller>,
    ) -> Result<Home<'static>> {
        if let Some(workspace) = (launch.in_workspace.as_ref()).and_then(SpaceChoice::named) {
            return Ok(Home::Worktree(workspace.clone()));
        }
        if !shape.repository {
            return Ok(Home::Root);
        }
        let repository =
            (self.launch_chat_repository(daemon, launch, named, issue, caller)).await?;
        Ok(Home::Checkout(repository))
    }

    /// The one `named` names, else the repository of the `issue` URL, else
    /// that of the space it goes below, which a child shares, else that of
    /// the checkout or worktree the start was made in.
    async fn launch_chat_repository(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        named: Option<String>,
        issue: Option<&ItemInput>,
        caller: Option<&Caller>,
    ) -> Result<RepositoryId> {
        let repos = daemon.repositories().await?;
        if let Some(named) = named {
            let selector = crate::repository::named_in(named, launch.directory.as_deref())?;
            return Ok(crate::repository::select(&repos, selector)
                .await?
                .id
                .clone());
        }
        if let Some(ItemInput::Url(item)) = issue {
            item.of_kind(ItemKind::Issue.into())?;
            let retry = "shoal start --mode chat --issue <url> --repo <repository>";
            let forge = item.repository().clone();
            return Ok(crate::repository::with_remote(&repos, forge, retry)
                .await?
                .id
                .clone());
        }
        let hint = match issue {
            Some(_) => "pass --repo <repository> or a forge URL",
            None => "pass <repository>",
        };
        let state = self.launch_state(daemon, launch, caller).await?;
        if let Some(parent) = state.parent.here()? {
            return Ok(parent);
        }
        (launches::shape::directory_repository(launch, &repos, &state.workspaces).await?)
            .with_context(|| format!("no current registered repository; {hint}"))
    }
}

/// Refuses a repository's setup, which works on its setup branch, and for a
/// chat without a repository what only one with a repository takes.
fn ensure_chat_takes(launch: &Launch, shape: &Shape) -> Result<()> {
    ensure!(
        launch.setup != Some(Setup::Repository),
        "a repository's setup starts in a workspace on its setup branch"
    );
    if !shape.repository {
        launches::ensure_no_repository(launch, &shape.mode)?;
    }
    Ok(())
}

/// Only one the launch names, since a pick needs a terminal. It stays open
/// where the launch `picks`.
fn named_issue(issue: Option<ItemChoice>, picks: bool) -> Result<Option<ItemInput>> {
    match issue {
        Some(ItemChoice::Pick) => {
            ensure!(
                picks,
                "missing issue; pass --issue <number or URL> in non-interactive mode"
            );
            Ok(None)
        }
        Some(ItemChoice::Named(issue)) => Ok(Some(issue)),
        None => Ok(None),
    }
}

pub mod check;
