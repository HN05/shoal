//! A launch that starts its agent in a new chat: in a repository's
//! checkout, in the worktree of the workspace it names, or without a
//! repository in `root_dir`. The chat is recorded below where it goes and
//! linked to the issue it starts on, and its agent queues as any space's
//! does, which the queue starts in a background place.
use anyhow::{Context as _, Result, ensure};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::launches::{IssueChoice, Launch, LaunchSource, Launched, Setup, SpaceChoice};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

use super::shape::Shape;
use crate::{
    agent::Agent,
    agents::{NO_AGENT, checked_agent},
    config::{Effective, modes::ModeName},
    daemon::{
        chats::NewChat,
        links::issue::{AssignerOf, OpenIssue},
        notifications::NotificationKind,
        scope::{Caller, ScopeRefusal},
        workspace::Manager,
    },
    forge::items::{
        link::ItemKind,
        reference::{ItemInput, ItemRef},
    },
    launches,
    model::{Assigner, Chat, ChatHome, QueueAdmission, SpaceKind},
    protocol::operations::{LaunchIssue, QueuedLaunch},
};

/// Where a new chat runs, which gives it its repository.
enum Home {
    /// A registered repository's checkout.
    Checkout(RepositoryId),
    /// The worktree of a ready workspace, in that workspace's repository.
    Worktree(SpaceSelector),
    /// `root_dir`, without a repository.
    Root,
}

/// A new chat as its launch resolves it: where it runs and below which
/// space, its name, its agent and mode, and the issue it starts on.
struct NewChatStart {
    home: Home,
    repository: Option<RepositoryId>,
    parent: Option<SpaceId>,
    name: SpaceName,
    agent: Agent,
    mode: ModeName,
    issue: Option<StartIssue>,
}

/// The issue a new chat starts on, by its URL, with the title and labels it
/// was read open with, unless a launch that names the chat could not read
/// it.
struct StartIssue {
    url: ItemRef,
    title: Option<String>,
    labels: Vec<String>,
}

impl Manager {
    /// Start the agent of `launch`, which `source` made and `shape` starts
    /// in a new chat, for `caller`: record the chat below where it goes,
    /// link it to the issue it starts on and queue its agent, which the
    /// queue starts in a background place, at once where it admits it.
    pub(super) async fn launch_new_chat(
        &self,
        launch: Launch,
        shape: Shape,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        ensure_chat_takes(&launch, &shape)?;
        let in_workspace = launch.in_workspace.is_some();
        let (named, issue) = launches::chat_target(
            launch.target.clone(),
            launch.repository.clone(),
            launch.issue.clone(),
            in_workspace,
        )?;
        let issue = named_issue(issue, &shape.mode)?;
        let home =
            (self.launch_chat_home(&launch, &shape, (named, issue.as_ref()), caller)).await?;
        let start = (self.new_chat_start(&launch, home, &shape, issue.as_ref(), caller)).await?;
        self.ensure_chat_allowed(&launch, &start, caller).await?;
        let chat = self.record_launched_chat(&launch, &start, caller).await?;
        let warnings = self.link_chat(&chat, &start, caller).await;
        let chat = (self.chat_by_id(&chat.id).await?).context("the new chat was removed")?;
        let admission = self.queue_chat_agent(&chat, launch, &start, source).await?;
        Ok(Launched {
            space: Box::new(chat.into()),
            entry: admission.entry.map(Box::new),
            warnings,
            start: shoal_protocol::launches::Started::Queued {
                waiting: admission.waiting,
            },
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
    ) -> Result<Home> {
        if let Some(workspace) = (launch.in_workspace.as_ref()).and_then(SpaceChoice::named) {
            return Ok(Home::Worktree(workspace.clone()));
        }
        if !shape.repository {
            return Ok(Home::Root);
        }
        let repository = (self.launch_chat_repository(launch, named, issue, caller)).await?;
        Ok(Home::Checkout(repository))
    }

    /// The chat `launch` starts at `home` in `shape`, for `caller`: below
    /// the parent it names, else `caller`'s own space, with the agent it
    /// names, else `agent.default`, on the open `issue` it names, under the
    /// name it gives, else that issue's title, else that of its home.
    async fn new_chat_start(
        &self,
        launch: &Launch,
        home: Home,
        shape: &Shape,
        issue: Option<&ItemInput>,
        caller: Option<&Caller>,
    ) -> Result<NewChatStart> {
        let repository = match &home {
            Home::Checkout(repository) => Some(repository.clone()),
            Home::Worktree(workspace) => self.chat_workspace(workspace).await?.repository_id,
            Home::Root => None,
        };
        let parent = match self.space_id(launch.parent.as_ref()).await? {
            Some(parent) => Some(parent),
            None => caller.map(|caller| caller.space_id.clone()),
        };
        let selector = repository.as_ref().map(Into::into);
        (self.ensure_parent_takes(selector.as_ref(), parent.as_ref())).await?;
        let settings = self.new_chat_settings(repository.as_ref()).await?;
        let agent =
            checked_agent(&settings, launch.agent.clone(), &launch.model)?.context(NO_AGENT)?;
        let in_repository = repository.is_some();
        let mode = shape.mode.clone();
        crate::config::modes::resolve(&settings.modes, &mode, SpaceKind::Chat, in_repository)?;
        let issue = (self.launch_issue(launch, issue, repository.as_ref())).await?;
        let title = issue.as_ref().and_then(|issue| issue.title.as_ref());
        let name = match (&launch.name, title) {
            (Some(name), _) => name.clone(),
            (None, Some(title)) => SpaceName::derive(title, "chat"),
            (None, None) => self.home_chat_name(&home, &mode).await?,
        };
        Ok(NewChatStart {
            home,
            repository,
            parent,
            name,
            agent,
            mode,
            issue,
        })
    }

    /// The issue `input` names, which the chat `launch` starts in
    /// `repository` starts on, read open. A launch that names the chat
    /// needs no title from it, so an issue it cannot read is taken by the
    /// URL its number has in the repository, which the chat's start reads
    /// again; one without even that is refused, since nothing could read it.
    async fn launch_issue(
        &self,
        launch: &Launch,
        input: Option<&ItemInput>,
        repository: Option<&RepositoryId>,
    ) -> Result<Option<StartIssue>> {
        let Some(input) = input else {
            return Ok(None);
        };
        let repository = repository
            .context("a chat without a repository starts on no issue; name its repository")?;
        let checkout = self.repository(repository.clone()).await?.path;
        let error = match self.read_open_issue(&checkout, input).await {
            Ok(OpenIssue { url, title, labels }) => {
                let title = Some(title);
                return Ok(Some(StartIssue { url, title, labels }));
            }
            Err(error) if launch.name.is_some() => error,
            Err(error) => return Err(error),
        };
        crate::daemon::log!("chat issue {input} read without its title: {error:#}");
        let (_, url) = self.issue_forge(&checkout, input).await?;
        let labels = Vec::new();
        Ok(Some(StartIssue {
            url,
            title: None,
            labels,
        }))
    }

    /// The name of a new chat in `mode` at `home` that names none.
    async fn home_chat_name(&self, home: &Home, mode: &ModeName) -> Result<SpaceName> {
        let name = match home {
            Home::Worktree(workspace) => {
                let workspace = self.chat_workspace(workspace).await?;
                ChatHome::Workspace(&workspace).chat_name(mode)
            }
            Home::Checkout(repository) => {
                let repository = self.repository(repository.clone()).await?;
                ChatHome::Checkout(&repository).chat_name(mode)
            }
            Home::Root => {
                let root_dir = self.config().root_dir(&self.paths)?;
                ChatHome::Root(&root_dir).chat_name(mode)
            }
        };
        Ok(SpaceName::derive(&name, "chat"))
    }

    /// Refuse the chat `start` that `launch` describes where `caller`'s
    /// scope or permissions do not allow it: a scoped caller's chat goes
    /// below its own space, or one below it, never below the workspace it
    /// runs in, and takes the caller's slot rules and its permissions.
    async fn ensure_chat_allowed(
        &self,
        launch: &Launch,
        start: &NewChatStart,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let workspace = match &start.home {
            Home::Worktree(workspace) => Some(workspace),
            Home::Checkout(_) | Home::Root => None,
        };
        // A named parent is never the workspace the chat runs in; one it
        // leaves out is the caller's own space, wherever the chat runs.
        (self.ensure_parent_not_host(workspace, launch.parent.as_ref())).await?;
        if let Some(caller) = caller {
            let mut parent = start.parent.as_ref().map(SpaceSelector::id);
            ScopeRefusal::of(self.parent_below(caller, &mut parent).await)?;
            if launch.now {
                let now = format!("start the agent of new chat {} now", start.name);
                let repository = start.repository.clone();
                ScopeRefusal::of(self.authorize_beyond_limits(caller, now, repository).await)?;
            }
        }
        let repository = start.repository.as_ref().map(Into::into);
        let place = (
            repository.as_ref().filter(|_| workspace.is_none()),
            workspace,
        );
        (self.ensure_chat_given_held(caller, &launch.permissions, place)).await
    }

    /// Record the new chat `start` describes, with the priority `launch`
    /// gives it, at most `caller`'s own.
    async fn record_launched_chat(
        &self,
        launch: &Launch,
        start: &NewChatStart,
        caller: Option<&Caller>,
    ) -> Result<Chat> {
        let priority = self.given_priority(caller, launch.priority).await?;
        let new = NewChat {
            name: start.name.clone(),
            agent: start.agent.clone(),
            mode: start.mode.clone(),
            narrowing: launch.permissions,
            origin: None,
            parent: start.parent.clone(),
            priority,
        };
        let (repository, workspace) = match &start.home {
            Home::Checkout(repository) => (Some(repository.into()), None),
            Home::Worktree(workspace) => (None, Some(workspace)),
            Home::Root => (None, None),
        };
        self.start_new_chat(repository, workspace, new).await
    }

    /// Link the recorded `chat` for `caller` to the issue `start` starts
    /// on, assigned, telling its starter what that warns of, which it
    /// returns.
    async fn link_chat(
        &self,
        chat: &Chat,
        start: &NewChatStart,
        caller: Option<&Caller>,
    ) -> Vec<String> {
        let Some(issue) = &start.issue else {
            return Vec::new();
        };
        let (selector, input) = (SpaceSelector::id(&chat.id), issue.url.clone().into());
        let title = issue.title.clone();
        let warnings = match self
            .set_issue(&selector, &input, title, Assigner::of(caller))
            .await
        {
            Ok(warnings) => warnings,
            Err(error) => vec![format!("cannot link issue {input}: {error:#}")],
        };
        if !warnings.is_empty() {
            let message = format!("starting {} warned: {}", chat.name, warnings.join("; "));
            let kind = NotificationKind::StartWarned;
            self.notify_space(&chat.name, &chat.id, kind, message).await;
        }
        warnings
    }

    /// Queue the agent `launch` starts in the recorded `chat`, as `start`
    /// resolved it, which `source` made, on the issue it starts on.
    async fn queue_chat_agent(
        &self,
        chat: &Chat,
        launch: Launch,
        start: &NewChatStart,
        source: LaunchSource,
    ) -> Result<QueueAdmission> {
        let issue = (start.issue.as_ref()).map(|issue| LaunchIssue {
            url: issue.url.clone(),
            title: (issue.title.clone()).unwrap_or_else(|| issue.url.to_string()),
            labels: issue.labels.clone(),
        });
        let queued = QueuedLaunch {
            prompt: launch.prompt,
            mode: Some(start.mode.clone()),
            narrowing: launch.permissions,
            model: launch.model,
            args: launch.task,
            issue,
            env: launch.env,
            now: launch.now,
            source: Some(source),
            ..QueuedLaunch::new(start.agent.clone())
        };
        let request = crate::start_plan::queued_agent_request(chat, queued, None)?;
        self.queue_add(request).await
    }

    /// The settings a new chat of `repository` starts with, those of the
    /// global config alone for one without a repository, with the mode
    /// files beside the global config.
    async fn new_chat_settings(&self, repository: Option<&RepositoryId>) -> Result<Effective> {
        match repository {
            Some(id) => self.repository_settings_with_templates(id).await,
            None => (self.config()).resolve_with_template_files(&Default::default(), &self.paths),
        }
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
