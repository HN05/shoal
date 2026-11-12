//! New chats the daemon starts for a client, as `shoal start` starts one
//! without a terminal: it resolves what the start leaves out under the
//! chat's settings and checks where the chat goes, then runs the CLI's chat
//! start worker, which opens a background place for it, and answers once
//! the chat runs its agent.
use anyhow::{Context as _, Result, ensure};

use crate::client::{repository_ids::RepositoryId, scope_tokens::ScopeToken};
use crate::{
    agents::{NO_AGENT, checked_agent},
    client::output::chat::StartedChat,
    config::Effective,
    daemon::{scope::Caller, workspace::Manager},
    model::SpaceKind,
    protocol::ChatStart,
};

impl Manager {
    /// Start the chat `start` describes for `caller` in a background place,
    /// with its agent, mode and repository resolved. The worker acts with
    /// `caller`'s scope, so a scoped caller's chat goes where its own start
    /// would put it.
    pub(crate) async fn launch_chat(
        &self,
        start: ChatStart,
        caller: Option<&Caller>,
    ) -> Result<StartedChat> {
        let start = self.resolve_chat_start(start, caller).await?;
        let token = caller.map(|caller| self.operations.issue(caller));
        let started = run_worker(&self.paths, &start, token.as_ref()).await;
        if let Some(token) = &token {
            self.operations.revoke(token);
        }
        started
    }

    /// `start` with its repository named by ID and the agent and mode it
    /// names, else `agent.default` and the chat mode, checked under the
    /// chat's settings, refused where its parent, else `caller`'s own space,
    /// takes no such child.
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
        let parent = match self.space_id(start.parent.as_ref()).await? {
            Some(parent) => Some(parent),
            None => caller.map(|caller| caller.space_id.clone()),
        };
        let selector = repository.as_ref().map(Into::into);
        (self.ensure_parent_takes(selector.as_ref(), parent.as_ref())).await?;
        let settings = self.new_chat_settings(repository.as_ref()).await?;
        let agent =
            checked_agent(&settings, start.agent.clone(), &start.model)?.context(NO_AGENT)?;
        let mode = (start.mode.clone()).unwrap_or_else(crate::model::chat_mode);
        let in_repository = repository.is_some();
        crate::config::modes::resolve(&settings.modes, &mode, SpaceKind::Chat, in_repository)?;
        // A chat in a workspace's worktree is in that workspace's repository.
        let named = repository.filter(|_| start.workspace.is_none());
        Ok(ChatStart {
            repository: named.map(Into::into),
            agent: Some(agent),
            mode: Some(mode),
            ..start
        })
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

/// Run the CLI's chat start worker for the resolved `start`, detached from
/// the daemon in the environment of the terminal it names, as the user or,
/// with `token`, with that scope, and return the chat it reports once it
/// runs its agent.
#[cfg(not(test))]
async fn run_worker(
    paths: &crate::paths::Paths,
    start: &ChatStart,
    token: Option<&ScopeToken>,
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
