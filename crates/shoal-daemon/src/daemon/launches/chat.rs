//! A launch that starts its agent in a new chat: in a repository's
//! checkout, in the worktree of the workspace it names, or without a
//! repository in `root_dir`.
use anyhow::{Context as _, Result, ensure};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::chats::StartedChat;
use shoal_protocol::launches::{IssueChoice, Launch, LaunchSource, SpaceChoice};

use super::shape::Shape;
use crate::{
    config::modes::ModeName,
    daemon::{scope::Caller, workspace::Manager},
    forge::items::{link::ItemKind, reference::ItemInput},
    launches,
    protocol::ChatStart,
};

impl Manager {
    /// Start the agent of `launch`, which `source` made and `shape`
    /// starts in a chat, for `caller`.
    pub(super) async fn launch_in_chat(
        &self,
        launch: Launch,
        shape: Shape,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<StartedChat> {
        let start = self.chat_start(launch, &shape, caller).await?;
        self.ensure_chat_start_allowed(&start, caller).await?;
        self.launch_chat(start, source, caller).await
    }

    /// The chat start of `launch` in `shape`: where the chat runs, on which
    /// issue, and what its agent runs with; the chat's own start resolves
    /// what it leaves out.
    async fn chat_start(
        &self,
        launch: Launch,
        shape: &Shape,
        caller: Option<&Caller>,
    ) -> Result<ChatStart> {
        if !shape.repository {
            launches::ensure_no_repository(&launch, &shape.mode)?;
        }
        let workspace = (launch.in_workspace.as_ref()).and_then(SpaceChoice::named);
        let (named, issue) = launches::chat_target(
            launch.target.clone(),
            launch.repository.clone(),
            launch.issue.clone(),
            workspace.is_some(),
        )?;
        let issue = named_issue(issue, &shape.mode)?;
        let repository = match (shape.repository, workspace) {
            (true, None) => Some(
                self.launch_chat_repository(&launch, named, issue.as_ref(), caller)
                    .await?,
            ),
            _ => None,
        };
        Ok(ChatStart {
            repository: repository.map(Into::into),
            workspace: workspace.cloned(),
            parent: launch.parent,
            name: launch.name,
            agent: launch.agent,
            mode: Some(shape.mode.clone()),
            model: launch.model,
            narrowing: launch.permissions,
            issue,
            args: launch.task,
            priority: launch.priority,
            env: launch.env,
            now: launch.now,
        })
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
            let selector = crate::repository::named(named)?;
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
        (self.directory_repository(launch, &repos, caller).await?)
            .with_context(|| format!("no current registered repository; {hint}"))
    }

    /// Refuse the chat `start` where its parent cannot take it, or where
    /// `caller`'s scope or permissions do not allow it: a scoped caller's
    /// chat goes below its own space, or one below it, never below the
    /// workspace it runs in, and takes the caller's slot rules and its
    /// permissions.
    async fn ensure_chat_start_allowed(
        &self,
        start: &ChatStart,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let (workspace, parent) = (start.workspace.as_ref(), start.parent.as_ref());
        self.ensure_parent_not_host(workspace, parent).await?;
        if let Some(caller) = caller {
            self.parent_below(caller, &mut start.parent.clone()).await?;
            if start.now {
                self.authorize_chat_now(caller, start).await?;
            }
        }
        let place = (start.repository.as_ref(), start.workspace.as_ref());
        (self.ensure_chat_given_held(caller, &start.narrowing, place)).await
    }
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
