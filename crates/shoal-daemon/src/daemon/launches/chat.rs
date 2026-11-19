//! Where a launch's new chat goes: in a repository's checkout, in the
//! worktree of the workspace it names, or without a repository in
//! `root_dir`, and the issue it starts on, which a chat takes only as the
//! launch names it.
use anyhow::{Context as _, Result, ensure};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::{IssueChoice, Launch, Setup, SpaceChoice};

use super::{
    shape::Shape,
    space::{PlacedHome, Placement},
};
use crate::{
    config::modes::ModeName,
    daemon::{manager::Manager, operations::starts::Home, scope::Caller},
    forge::items::{link::ItemKind, reference::ItemInput},
    launches,
};

impl Manager {
    /// Where the new chat `launch` starts in `shape` goes for `caller`,
    /// with the issue it starts on: a chat always starts its agent.
    pub(super) async fn chat_placement(
        &self,
        launch: &Launch,
        shape: &Shape,
        caller: Option<&Caller>,
    ) -> Result<Placement> {
        ensure_chat_takes(launch, shape)?;
        let in_workspace = launch.in_workspace.is_some();
        let (named, issue) = launches::chat_target(
            launch.target.clone(),
            launch.repository.clone(),
            launch.issue.clone(),
            in_workspace,
        )?;
        let issue = named_issue(issue, &shape.mode)?;
        let home = (self.launch_chat_home(launch, shape, (named, issue.as_ref()), caller)).await?;
        // A named parent is never the workspace the chat runs in; one it
        // leaves out is the caller's own space, wherever the chat runs.
        if let Home::Worktree(workspace) = &home {
            (self.ensure_parent_not_host(Some(workspace), launch.parent.as_ref())).await?;
        }
        let issue = match issue {
            Some(issue) => {
                let repository = (self.home_repository(&home).await?).context(
                    "a chat without a repository starts on no issue; name its repository",
                )?;
                // A chat its launch names needs no title from its issue.
                let by_url = launch.name.is_some();
                let read = self.launch_issue(&issue, (&repository, by_url), caller);
                Some(read.await?)
            }
            None => None,
        };
        Ok(Placement {
            home: PlacedHome::Chat(home),
            issue,
            agent: true,
            blockers: Vec::new(),
        })
    }

    /// Where the chat `launch` starts runs: in the worktree of the
    /// workspace it names, without a repository for a `shape` that has
    /// none, else in the checkout of the repository it `names`, or that of
    /// the issue it starts on or of where the start was made.
    async fn launch_chat_home(
        &self,
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
        let repository = (self.launch_chat_repository(launch, named, issue, caller)).await?;
        Ok(Home::Checkout(repository))
    }

    /// The registered repository of the chat `launch` starts, which
    /// `named` names, else the repository of the `issue` URL it starts on,
    /// else that of the checkout or worktree the start was made in.
    async fn launch_chat_repository(
        &self,
        launch: &Launch,
        named: Option<String>,
        issue: Option<&ItemInput>,
        caller: Option<&Caller>,
    ) -> Result<RepositoryId> {
        let repos = self.repositories().await?;
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
        let reads = super::ManagerReads::new(self, caller);
        (launches::shape::directory_repository(&reads, launch, &repos).await?)
            .with_context(|| format!("no current registered repository; {hint}"))
    }
}

/// Refuse what a chat of `shape` cannot start with: running its agent in
/// the caller, which a launch never does for a chat, a repository's setup,
/// which works on its setup branch, and for a chat without a repository
/// what only one with a repository takes.
fn ensure_chat_takes(launch: &Launch, shape: &Shape) -> Result<()> {
    ensure!(
        !launch.here,
        "a launch runs no chat's agent in its caller; start the chat here with `shoal start --here`"
    );
    ensure!(
        launch.setup != Some(Setup::Repository),
        "a repository's setup starts in a workspace on its setup branch"
    );
    if !shape.repository {
        launches::ensure_no_repository(launch, &shape.mode)?;
    }
    Ok(())
}

/// The issue a chat in `mode` starts on: one the launch names, since a
/// pick needs a terminal.
fn named_issue(issue: Option<IssueChoice>, mode: &ModeName) -> Result<Option<ItemInput>> {
    match issue {
        Some(IssueChoice::Pick) => {
            ensure!(
                *mode == ModeName::PLAN,
                "--issue needs a number or URL outside plan mode"
            );
            anyhow::bail!("missing issue; pass --issue <number or URL> in non-interactive mode")
        }
        Some(IssueChoice::Named(issue)) => Ok(Some(issue)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests;
