//! New chats the daemon starts for a client, as `shoal start` starts one
//! without a terminal: it resolves what the start leaves out under the
//! chat's settings and checks where the chat goes, and admits its agent
//! under the agent slots. An admitted chat's start worker opens a background
//! place for it, and the daemon answers once the chat runs its agent; one
//! whose agent waits is recorded at once, and the answer names its entry.
use anyhow::{Context as _, Result, ensure};

use crate::client::{
    repository_ids::RepositoryId, scope_tokens::ScopeToken, space_ids::SpaceId,
    space_names::SpaceName,
};
use crate::{
    agents::{NO_AGENT, checked_agent},
    client::output::chat::{QueuedChat, StartedChat},
    config::{Effective, modes::ModeName},
    daemon::{scope::Caller, workspace::Manager},
    model::{ChatHome, ChatQueueRequest, QueueAdmission, SpaceKind},
    protocol::ChatStart,
};
use shoal_core::queued_starts::{ChatPlan, ChatPlanStart};

impl Manager {
    /// Start the chat `start` describes for `caller` in a background place,
    /// with its agent, mode and repository resolved, once the agent queue
    /// admits its agent; one that waits is recorded below where it goes
    /// and starts from the queue. The worker acts with `caller`'s scope, so
    /// a scoped caller's chat goes where its own start would put it.
    pub(crate) async fn launch_chat(
        &self,
        start: ChatStart,
        caller: Option<&Caller>,
    ) -> Result<StartedChat> {
        let start = self.resolve_chat_start(start, caller).await?;
        let admission = self.queue_chat_add(launch_request(&start)?, caller).await?;
        if !admission.start {
            return self.record_queued_chat(start, admission, caller).await;
        }
        let entry = admission.entry.map(|entry| entry.id);
        let token = caller.map(|caller| self.operations.issue(caller));
        let started = run_worker(&self.paths, &start, token.as_ref(), entry.as_ref()).await;
        if let Some(token) = &token {
            self.operations.revoke(token);
        }
        started
    }

    /// `start` with its repository named by ID and the agent, mode and name
    /// it names, else `agent.default`, the chat mode and a name of its own,
    /// checked under the chat's settings, refused where its parent, else
    /// `caller`'s own space, takes no such child.
    async fn resolve_chat_start(
        &self,
        start: ChatStart,
        caller: Option<&Caller>,
    ) -> Result<ChatStart> {
        let repository = self.new_chat_repository(&start).await?;
        ensure!(
            repository.is_some() || start.issue.is_none(),
            "a chat without a repository starts on no issue; name its repository"
        );
        let parent = self.new_chat_parent(&start, caller).await?;
        let selector = repository.as_ref().map(Into::into);
        (self.ensure_parent_takes(selector.as_ref(), parent.as_ref())).await?;
        let settings = self.new_chat_settings(repository.as_ref()).await?;
        let agent =
            checked_agent(&settings, start.agent.clone(), &start.model)?.context(NO_AGENT)?;
        let mode = (start.mode.clone()).unwrap_or_else(crate::model::chat_mode);
        let in_repository = repository.is_some();
        crate::config::modes::resolve(&settings.modes, &mode, SpaceKind::Chat, in_repository)?;
        let name = match start.name.clone() {
            Some(name) => name,
            None => {
                self.new_chat_name(&start, repository.as_ref(), &mode)
                    .await?
            }
        };
        // A chat in a workspace's worktree is in that workspace's repository.
        let named = repository.filter(|_| start.workspace.is_none());
        Ok(ChatStart {
            repository: named.map(Into::into),
            agent: Some(agent),
            mode: Some(mode),
            name: Some(name),
            ..start
        })
    }

    /// The space the new chat `start` describes goes below: the one it
    /// names, else `caller`'s own.
    async fn new_chat_parent(
        &self,
        start: &ChatStart,
        caller: Option<&Caller>,
    ) -> Result<Option<SpaceId>> {
        Ok(match self.space_id(start.parent.as_ref()).await? {
            Some(parent) => Some(parent),
            None => caller.map(|caller| caller.space_id.clone()),
        })
    }

    /// Record the chat the resolved `start` describes, whose agent's launch
    /// `admission` queued, below where it goes for `caller`, and attach the
    /// launch to it, keeping the launch out of admission meanwhile so that
    /// it starts in this chat. A chat that cannot be recorded takes its
    /// launch out of the queue.
    async fn record_queued_chat(
        &self,
        start: ChatStart,
        admission: QueueAdmission,
        caller: Option<&Caller>,
    ) -> Result<StartedChat> {
        let entry = (admission.entry).context("the daemon queued the chat without an entry")?;
        self.agent_queue.hold(&entry.id);
        let recorded = self.record_chat_for_queue(&start, &entry.id, caller).await;
        self.agent_queue.release(&entry.id);
        let chat = match recorded {
            Ok(chat) => chat,
            Err(error) => {
                if let Err(removal) = self.queue_remove(entry.id.as_str(), None).await {
                    crate::daemon::log!("queued chat launch kept: {removal:#}");
                }
                return Err(error);
            }
        };
        self.agent_queue.wake();
        Ok(StartedChat::Queued(Box::new(QueuedChat {
            chat,
            entry,
            waiting: admission.waiting,
        })))
    }

    /// Record the chat the resolved `start` describes below where it goes
    /// for `caller`, and attach the launch `entry` to it.
    async fn record_chat_for_queue(
        &self,
        start: &ChatStart,
        entry: &crate::client::queue::QueueEntryId,
        caller: Option<&Caller>,
    ) -> Result<crate::model::Chat> {
        let new = super::NewChat {
            name: start.name.clone().context("the start names the chat")?,
            agent: start.agent.clone().context(NO_AGENT)?,
            mode: start.mode.clone().unwrap_or_else(crate::model::chat_mode),
            narrowing: start.narrowing,
            origin: None,
            parent: self.new_chat_parent(start, caller).await?,
            priority: start.priority,
        };
        let workspace = start.workspace.as_ref();
        let chat = (self.start_new_chat(start.repository.clone(), workspace, new)).await?;
        self.queue_attach(
            entry,
            &crate::client::space_names::SpaceSelector::id(&chat.id),
        )
        .await?;
        Ok(chat)
    }

    /// The name of the new chat `start` describes, in `mode` and
    /// `repository`, when it names none: the title of the open issue it
    /// starts on, as a new workspace takes it, else the name of where it
    /// runs.
    async fn new_chat_name(
        &self,
        start: &ChatStart,
        repository: Option<&RepositoryId>,
        mode: &ModeName,
    ) -> Result<SpaceName> {
        let name = match (&start.workspace, repository, &start.issue) {
            (_, Some(repository), Some(issue)) => {
                let checkout = self.repository(repository.clone()).await?.path;
                self.read_open_issue(&checkout, issue).await?.title
            }
            (Some(workspace), ..) => {
                let workspace = self.chat_workspace(workspace).await?;
                ChatHome::Workspace(&workspace).chat_name(mode)
            }
            (None, Some(repository), None) => {
                let repository = self.repository(repository.clone()).await?;
                ChatHome::Checkout(&repository).chat_name(mode)
            }
            (None, None, _) => {
                let root_dir = self.config().root_dir(&self.paths)?;
                ChatHome::Root(&root_dir).chat_name(mode)
            }
        };
        Ok(SpaceName::derive(&name, "chat"))
    }

    /// The repository of the chat `start` describes: that of the ready
    /// workspace it runs in, else the one it names; none for a chat without
    /// one.
    async fn new_chat_repository(&self, start: &ChatStart) -> Result<Option<RepositoryId>> {
        Ok(match (&start.workspace, &start.repository) {
            (Some(workspace), _) => Some(self.chat_workspace(workspace).await?.repository_id),
            (None, Some(repository)) => Some(self.repository(repository.clone()).await?.id),
            (None, None) => None,
        })
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
}

/// The launch of the resolved `start`'s agent to admit, whose plan, once
/// it waited, starts the chat the daemon recorded meanwhile.
fn launch_request(start: &ChatStart) -> Result<ChatQueueRequest> {
    let plan = ChatPlan {
        parent: start.parent.clone(),
        chat_start: ChatPlanStart::Resolved(Box::new(start.clone())),
    };
    Ok(ChatQueueRequest {
        repository: start.repository.clone(),
        name: start.name.clone().context("the start names the chat")?,
        agent: start.agent.clone().context(NO_AGENT)?,
        mode: start.mode.clone().unwrap_or_else(crate::model::chat_mode),
        narrowing: start.narrowing,
        plan: serde_json::to_string(&plan)?,
        env: (start.env.clone())
            .unwrap_or_else(|| crate::terminal::detached_start_environment(None)),
        now: false,
        priority: start.priority,
    })
}

/// Run the chat start worker for the resolved `start`, detached from
/// the daemon in the environment of the terminal it names, as the user or,
/// with `token`, with that scope, its agent taking the slot of the queue
/// `entry`, and return the chat it reports once it runs its agent.
#[cfg(not(test))]
async fn run_worker(
    paths: &crate::paths::Paths,
    start: &ChatStart,
    token: Option<&ScopeToken>,
    entry: Option<&crate::client::queue::QueueEntryId>,
) -> Result<StartedChat> {
    use crate::workers::{Worker, detached_command, internal_command};
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt as _;

    let argv = internal_command(paths, true, Worker::ChatStart)?;
    let env =
        (start.env.clone()).unwrap_or_else(|| crate::terminal::detached_start_environment(None));
    let mut command = detached_command(&argv);
    command
        .envs(&env)
        .current_dir(&paths.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match token {
        Some(token) => command.env(crate::env::SCOPE_TOKEN, token.expose()),
        None => command.env_remove(crate::env::SCOPE_TOKEN),
    };
    match entry {
        Some(entry) => command.env(crate::env::QUEUE_ENTRY, entry.as_str()),
        None => command.env_remove(crate::env::QUEUE_ENTRY),
    };
    let mut child = command.spawn().context("run the chat start worker")?;
    let mut stdin = child
        .stdin
        .take()
        .context("the worker has no standard input")?;
    stdin.write_all(&serde_json::to_vec(start)?).await?;
    drop(stdin);
    let output = child.wait_with_output().await?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    serde_json::from_slice(&output.stdout).context("read the chat the worker started")
}

/// Tests record the resolved starts, and whether the worker acts with a
/// scope, instead of running this binary as the CLI, and report each as
/// started in a background place.
#[cfg(test)]
async fn run_worker(
    _paths: &crate::paths::Paths,
    start: &ChatStart,
    token: Option<&ScopeToken>,
    _entry: Option<&crate::client::queue::QueueEntryId>,
) -> Result<StartedChat> {
    let scoped = token.is_some();
    tests::STARTED.lock().unwrap().push((start.clone(), scoped));
    Ok(StartedChat::Background(
        crate::client::output::chat::BackgroundChat {
            chat: start.name.clone().unwrap_or_else(|| "chat".into()),
            agent: start.agent.clone().context("the start names no agent")?,
            repository: None,
            place: "background".into(),
        },
    ))
}

#[cfg(test)]
pub(crate) mod tests;
