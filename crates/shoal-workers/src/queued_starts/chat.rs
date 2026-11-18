//! An admitted launch in a chat: the chat's worker starts its agent in a
//! background place where the terminal that queued it would have opened
//! one, and the agent takes the launch's slot. An agent that runs its own
//! way, such as Happy's, which needs no terminal host, starts in a chat
//! worker of this process instead.
use std::{path::PathBuf, process::Stdio};

use anyhow::{Context as _, Result};

use crate::{
    agent::Agent,
    chat_handoff::{HandedStart, Handoff},
    client::connection::CommandPaths,
    config::Effective,
    model::{Chat, Repository},
    protocol::{ChatStart, ConfigTarget, operations::QueuedLaunch},
    workers::{Worker, internal_command},
};
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// Start `launch`, the agent of the admitted launch `entry`, in the
/// recorded `chat`, its agent taking the launch's slot.
pub(super) async fn start(
    paths: &CommandPaths,
    entry: &QueueEntryId,
    chat: &Chat,
    launch: QueuedLaunch,
) -> Result<()> {
    crate::env::launch_queue_entry(entry);
    start_queued(paths, chat_start(chat, launch), chat.id.clone()).await
}

/// Start the agent `start` describes in `chat`, which was recorded for it,
/// as the user would.
async fn start_queued(paths: &CommandPaths, start: ChatStart, chat: SpaceId) -> Result<()> {
    let new = NewChat::resolve(paths, &start).await?;
    let handoff = Handoff::create(None)?;
    handoff.hand_start(&HandedStart { start, chat })?;
    match new.runs_its_own_way() {
        true => run_worker(paths, &handoff).await,
        false => new.start_in_background(paths, handoff).await,
    }
}

/// The start of `chat`'s agent as `launch` asks for it, where the chat
/// runs.
fn chat_start(chat: &Chat, launch: QueuedLaunch) -> ChatStart {
    let QueuedLaunch {
        agent,
        prompt,
        mode,
        narrowing,
        model,
        args,
        issue,
        now,
        ..
    } = launch;
    let workspace = chat.workdir.workspace().map(|workspace| &workspace.id);
    ChatStart {
        repository: (chat.repository_id.as_ref())
            .filter(|_| workspace.is_none())
            .map(Into::into),
        workspace: workspace.map(SpaceSelector::id),
        name: Some(chat.name.clone()),
        agent: Some(agent),
        mode,
        model,
        narrowing,
        issue: issue.map(|issue| issue.url.into()),
        prompt,
        args,
        now,
        ..ChatStart::default()
    }
}

/// A chat the daemon resolved, with where it runs and the settings it
/// starts under.
struct NewChat {
    name: SpaceName,
    agent: Agent,
    /// Its registered repository; none for a chat without one.
    repository: Option<Repository>,
    /// Its checkout, the worktree of the workspace it runs in, or
    /// `root_dir`.
    home: PathBuf,
    settings: Effective,
}

impl NewChat {
    /// The chat `start` describes, which the daemon named and gave its
    /// agent.
    async fn resolve(paths: &CommandPaths, start: &ChatStart) -> Result<Self> {
        let name = start.name.clone().context("the daemon names the chat")?;
        let agent = start
            .agent
            .clone()
            .context("the daemon names the chat's agent")?;
        let (repository, home) = home(paths, start).await?;
        let target = match &repository {
            Some(repository) => ConfigTarget::Repository(repository.id.clone().into()),
            None => ConfigTarget::Global,
        };
        let settings = crate::settings::of(paths, target).await?;
        Ok(Self {
            name,
            agent,
            repository,
            home,
            settings,
        })
    }

    /// Whether its agent runs its own way, leaving the process that starts
    /// it at once.
    fn runs_its_own_way(&self) -> bool {
        (crate::agents::agent_integration(&self.settings, &self.agent).detached()).is_some()
    }

    /// Open a background place whose chat worker starts this chat with the
    /// start `handoff` holds, and wait until the place reports that the
    /// chat runs.
    async fn start_in_background(self, paths: &CommandPaths, handoff: Handoff) -> Result<()> {
        let worker = internal_command(paths, false, chat_worker(&handoff))?;
        let repository = self.repository.as_ref();
        let (home, name) = (&self.home, self.name.as_str());
        crate::places::queued::start_chat(
            paths,
            &self.settings,
            repository,
            home,
            name,
            worker,
            false,
        )
        .await?;
        handoff.wait().await
    }
}

/// Where the chat `start` describes runs, with its registered repository:
/// the worktree of the workspace it names, else its repository's checkout,
/// else `root_dir`, without one.
async fn home(paths: &CommandPaths, start: &ChatStart) -> Result<(Option<Repository>, PathBuf)> {
    let repositories = crate::client::repositories::list(paths).await?;
    Ok(match (&start.workspace, &start.repository) {
        (Some(workspace), _) => {
            let selector = workspace.clone();
            let workspace = crate::client::workspaces::inspect(paths, selector)
                .await?
                .workspace;
            let repository = (repositories.into_iter())
                .find(|repository| repository.id == *workspace.repository())
                .context("the workspace's repository is no longer registered")?;
            (Some(repository), workspace.part.path)
        }
        (None, Some(selector)) => {
            let repository = crate::repository::select(&repositories, selector.clone()).await?;
            (Some(repository.clone()), repository.path.clone())
        }
        (None, None) => (None, crate::client::chats::root(paths).await?),
    })
}

/// Run the chat worker for the start `handoff` holds in no place, as a
/// process of this one, which ends once the agent runs its own way.
async fn run_worker(paths: &CommandPaths, handoff: &Handoff) -> Result<()> {
    let argv = internal_command(paths, true, chat_worker(handoff))?;
    let mut command = tokio::process::Command::new(&argv[0]);
    if let Some(entry) = crate::env::queue_entry() {
        command.env(crate::env::QUEUE_ENTRY, entry.as_str());
    }
    let status = command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("run the chat worker")?;
    let code = shoal_core::execution::exit_code(status);
    anyhow::ensure!(code == 0, "the chat's start failed ({code})");
    Ok(())
}

/// The chat worker that starts the chat whose start `handoff` holds.
fn chat_worker(handoff: &Handoff) -> Worker<'_> {
    Worker::Chat {
        handoff: handoff.path(),
    }
}
