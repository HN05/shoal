//! New chats the daemon starts for a client, as `shoal start` starts one
//! without a terminal: it resolves what the start leaves out under the
//! chat's settings and checks where the chat goes, admits its agent under
//! the heavy slots, and records the chat linked to what it starts on before
//! its agent runs. An admitted chat's start worker opens a background place
//! that runs the recorded chat's agent, and the daemon answers once it
//! runs; one whose agent waits starts from the queue.
use anyhow::{Context as _, Result, ensure};

use crate::{
    agents::{NO_AGENT, checked_agent},
    config::{Effective, modes::ModeName},
    daemon::{chats::NewChat, scope::Caller, workspace::Manager},
    model::{ChatHome, ChatQueueRequest, QueueAdmission, SpaceKind},
    protocol::ChatStart,
};
use shoal_core::chat_handoff::HandedStart;
use shoal_core::start_plan::{ChatPlan, ChatPlanStart};
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::chats::{LaunchedChat, QueuedChat, StartedChat};
use shoal_protocol::launches::LaunchSource;
use shoal_protocol::{scope_tokens::ScopeToken, space_names::SpaceName};

impl Manager {
    /// Start the chat `start` describes, which `source` made, for `caller`
    /// in a background place, with its agent, mode and repository
    /// resolved, once the agent queue admits its agent: the chat is
    /// recorded below where it goes and linked first, and one whose agent
    /// waits starts from the queue. The worker acts with `caller`'s scope.
    pub(crate) async fn launch_chat(
        &self,
        start: ChatStart,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<StartedChat> {
        let start = self.resolve_chat_start(start, caller).await?;
        let admission = (self.queue_chat_add(launch_request(&start, source)?, caller)).await?;
        let launched = self.record_admitted(&start, &admission, caller).await?;
        if !admission.start {
            return queued(launched, admission);
        }
        let entry = admission.entry.map(|entry| entry.id);
        self.start_in_place(start, launched, entry, caller).await
    }

    /// Run the agent of the chat `launched` recorded for the resolved
    /// `start` in a background place, its agent taking the slot of the
    /// admitted `entry`, with `caller`'s scope.
    async fn start_in_place(
        &self,
        start: ChatStart,
        launched: LaunchedChat,
        entry: Option<QueueEntryId>,
        caller: Option<&Caller>,
    ) -> Result<StartedChat> {
        let LaunchedChat { chat, warnings } = launched;
        let token = caller.map(|caller| self.operations.issue(caller));
        let handed = HandedStart {
            start,
            chat: Some(chat.id.clone()),
        };
        let started = run_worker(&self.paths, &handed, token.as_ref(), entry.as_ref()).await;
        if let Some(token) = &token {
            self.operations.revoke(token);
        }
        if let (Err(error), Some(entry)) = (&started, &entry) {
            self.free_failed_start(entry, error).await;
        }
        started.map(|started| warned(started, warnings))
    }

    /// Record the chat the resolved `start` describes for `caller` below
    /// where it goes, with the launch `admission` admitted or queued
    /// attached. A chat that cannot be recorded or attached takes its
    /// launch out of the queue.
    async fn record_admitted(
        &self,
        start: &ChatStart,
        admission: &QueueAdmission,
        caller: Option<&Caller>,
    ) -> Result<LaunchedChat> {
        let entry = admission.entry.as_ref().map(|entry| &entry.id);
        let recorded = async {
            let parent = self.new_chat_parent(start, caller).await?;
            let new = NewChat::of(start, parent, None)?;
            self.record_launch(start, new, entry, caller).await
        };
        let recorded = recorded.await;
        if let (Err(error), Some(entry)) = (&recorded, entry) {
            self.drop_unrecorded_launch(entry, admission.start, error)
                .await;
        }
        recorded
    }

    /// Take the launch `entry`, admitted when `started`, out of the queue,
    /// since its chat could not be recorded with `error`.
    async fn drop_unrecorded_launch(
        &self,
        entry: &QueueEntryId,
        started: bool,
        error: &anyhow::Error,
    ) {
        if started {
            return self.free_failed_start(entry, error).await;
        }
        self.agent_queue.end_attach_keep(entry);
        if let Err(removal) = self.queue_remove(entry.as_str(), None).await {
            crate::daemon::log!("queued chat launch kept: {removal:#}");
        }
    }

    /// Free the slot of the admitted launch `entry`, whose chat failed to
    /// start with `error`, rather than hold it until its grace ends.
    async fn free_failed_start(&self, entry: &QueueEntryId, error: &anyhow::Error) {
        if let Err(kept) = self.fail_queue_entry(entry, &format!("{error:#}")).await {
            crate::daemon::log!("chat launch {entry} keeps its slot: {kept:#}");
        }
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
    pub(in crate::daemon) async fn new_chat_repository(
        &self,
        start: &ChatStart,
    ) -> Result<Option<RepositoryId>> {
        Ok(match (&start.workspace, &start.repository) {
            (Some(workspace), _) => self.chat_workspace(workspace).await?.repository_id,
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
fn launch_request(start: &ChatStart, source: LaunchSource) -> Result<ChatQueueRequest> {
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
        now: start.now,
        priority: start.priority,
        records_when_waiting: true,
        source: Some(source),
    })
}

/// The chat `launched` recorded, whose agent waits as the launch
/// `admission` queued.
fn queued(launched: LaunchedChat, admission: QueueAdmission) -> Result<StartedChat> {
    let entry = (admission.entry).context("the daemon queued the chat without an entry")?;
    Ok(StartedChat::Queued(Box::new(QueuedChat {
        chat: launched.chat,
        entry,
        waiting: admission.waiting,
        warnings: launched.warnings,
    })))
}

/// The chat the daemon `started`, with what recording and linking it
/// `warnings` warned of.
fn warned(started: StartedChat, warnings: Vec<String>) -> StartedChat {
    match started {
        StartedChat::Queued(queued) => StartedChat::Queued(Box::new(QueuedChat {
            warnings,
            ..*queued
        })),
        StartedChat::Background(chat) => {
            StartedChat::Background(shoal_protocol::chats::BackgroundChat { warnings, ..chat })
        }
        StartedChat::Detached(chat) => {
            StartedChat::Detached(Box::new(shoal_protocol::chats::DetachedChat {
                warnings,
                ..*chat
            }))
        }
    }
}

/// Run the chat start worker for the resolved start `handed` holds,
/// detached from the daemon in the environment of the terminal it names,
/// as the user or, with `token`, with that scope, its agent taking the slot
/// of the queue `entry`, and return the chat it reports once it runs its
/// agent.
async fn run_worker(
    paths: &crate::paths::Paths,
    handed: &HandedStart,
    token: Option<&ScopeToken>,
    entry: Option<&shoal_ids::queue_entry_ids::QueueEntryId>,
) -> Result<StartedChat> {
    use crate::workers::{Worker, detached_command, internal_command};
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt as _;

    #[cfg(any(test, feature = "test-support"))]
    if crate::daemon::fake_workers::faked() {
        return fake::start(&handed.start, token);
    }

    let argv = internal_command(paths, true, Worker::ChatStart)?;
    let env = (handed.start.env.clone())
        .unwrap_or_else(|| crate::terminal::detached_start_environment(None));
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
    stdin.write_all(&serde_json::to_vec(handed)?).await?;
    drop(stdin);
    let output = child.wait_with_output().await?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    serde_json::from_slice(&output.stdout).context("read the chat the worker started")
}

#[cfg(any(test, feature = "test-support"))]
pub mod fake;
mod record;
