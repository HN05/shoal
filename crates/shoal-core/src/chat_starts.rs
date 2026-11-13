//! The daemon's worker for a new chat a client asked it to start, which has
//! no terminal: it hands the start the daemon resolved to a background
//! place, opened where the terminal that asked would have, whose chat
//! worker starts the chat there, and reports the chat once that place
//! reports that it runs. An agent that runs its own way, such as Happy's,
//! which needs no terminal host, starts in a chat worker of this process
//! instead, which reports the chat itself.
use std::{path::PathBuf, process::Stdio};

use anyhow::{Context as _, Result};

use crate::client::{output::chat::BackgroundChat, space_names::SpaceName};
use crate::{
    agent::Agent,
    chat_handoff::Handoff,
    config::Effective,
    model::Repository,
    paths::CommandPaths,
    protocol::{ChatStart, ConfigTarget},
    workers::{Worker, internal_command},
};

/// Start the chat whose resolved start the daemon writes to standard input,
/// reporting it on standard output.
pub async fn run(paths: &CommandPaths) -> Result<i32> {
    let start: ChatStart =
        serde_json::from_reader(std::io::stdin().lock()).context("read the chat's start")?;
    let chat = NewChat::resolve(paths, &start).await?;
    // A scoped caller's chat records itself with the caller's scope, which
    // the place takes through the handoff.
    let handoff = Handoff::create(crate::env::scope_token().as_ref())?;
    handoff.hand_start(&start)?;
    if chat.runs_its_own_way() {
        return run_worker(paths, &handoff).await;
    }
    let chat = chat.start_in_background(paths, handoff).await?;
    outln!("{}", crate::client::output::json(&chat)?);
    Ok(0)
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
        let settings = crate::config::settings(paths, target).await?;
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
    async fn start_in_background(
        self,
        paths: &CommandPaths,
        handoff: Handoff,
    ) -> Result<BackgroundChat> {
        let worker = internal_command(paths, false, chat_worker(&handoff))?;
        let repository = self.repository.as_ref();
        let (home, name) = (&self.home, self.name.as_str());
        let place = crate::terminal::queued::start_chat(
            paths,
            &self.settings,
            repository,
            home,
            name,
            worker,
            false,
        )
        .await?;
        handoff.wait().await?;
        Ok(BackgroundChat {
            repository: self.repository.map(|repository| repository.id),
            chat: self.name,
            agent: self.agent,
            place,
        })
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
                .find(|repository| repository.id == workspace.repository_id)
                .context("the workspace's repository is no longer registered")?;
            (Some(repository), workspace.path)
        }
        (None, Some(selector)) => {
            let repository = crate::repository::select(&repositories, selector.clone()).await?;
            (Some(repository.clone()), repository.path.clone())
        }
        (None, None) => (None, crate::client::chats::root(paths).await?),
    })
}

/// Run the chat worker for the start `handoff` holds in no place, as a
/// process of this one whose report on standard output stands for this
/// one's, returning its exit status.
async fn run_worker(paths: &CommandPaths, handoff: &Handoff) -> Result<i32> {
    let argv = internal_command(paths, true, chat_worker(handoff))?;
    let status = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .status()
        .await
        .context("run the chat worker")?;
    Ok(crate::execution::exit_code(status))
}

/// The chat worker that starts the chat whose start `handoff` holds.
fn chat_worker(handoff: &Handoff) -> Worker<'_> {
    Worker::Chat {
        handoff: handoff.path(),
    }
}
