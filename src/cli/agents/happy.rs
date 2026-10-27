//! Detached Happy sessions and their tool-specific prompt delivery.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use futures_util::future::BoxFuture;

use super::{ChatLaunch, Integration, Launch, Options, trust::TrustFile};
use crate::{
    ai::IntegrationKind,
    cli::context::Context,
    config::{self, Effective, templates},
    happy::{self, Agent},
    model::Workspace,
};

/// `happy-<agent>`: the agent in a detached Happy session.
pub(super) struct Happy(pub(super) Agent);

pub(super) const CLAUDE: Happy = Happy(Agent::Claude);
pub(super) const CODEX: Happy = Happy(Agent::Codex);

impl Happy {
    /// The integration of the agent Happy runs.
    fn inner(&self) -> &'static dyn Integration {
        super::integration(match self.0 {
            Agent::Claude => IntegrationKind::Claude,
            Agent::Codex => IntegrationKind::Codex,
        })
    }
}

impl Integration for Happy {
    /// Happy needs both its own CLI and the agent it runs.
    fn programs(&self) -> Option<Vec<&'static str>> {
        Some(vec!["happy", self.0.as_str()])
    }

    fn trust_file(&self) -> Option<&'static TrustFile> {
        self.inner().trust_file()
    }

    fn launches_itself(&self) -> bool {
        true
    }

    fn chat<'a>(
        &'a self,
        ctx: &'a Context,
        chat: &'a ChatLaunch,
    ) -> BoxFuture<'a, Result<Option<i32>>> {
        Box::pin(async move { super::detached::start_chat(ctx, self, chat).await.map(Some) })
    }

    fn chat_place(&self) -> Option<&'static str> {
        Some("the Happy app")
    }

    fn launch<'a>(
        &'a self,
        ctx: &'a Context,
        launch: &'a mut Launch,
    ) -> BoxFuture<'a, Result<Option<i32>>> {
        Box::pin(async move {
            let prompt = launch.prompt.take();
            let args = std::mem::take(&mut launch.args);
            super::detached::start(ctx, self, &launch.workspace, &launch.settings, prompt, args)
                .await
                .map(Some)
        })
    }
}

/// What Happy's section sets: `happy.enabled` turns Happy sessions on.
pub(super) fn options(config: &config::Happy) -> Options {
    Options {
        off: (!config.enabled).then_some(
            "Happy sessions are off; set happy.enabled = true in the repository or global config",
        ),
        ..Options::default()
    }
}

/// The first message and arguments of a session of `agent`, which runs
/// without Shoal's hooks. An agent that takes instructions apart from its
/// prompt gets them as arguments; Happy's Codex mode takes no arguments, so
/// they lead its first message.
pub(super) fn session_input(
    settings: &Effective,
    workspace: &Workspace,
    agent: Agent,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> (Option<String>, Vec<OsString>) {
    let instructions = templates::instructions(
        settings.worker_template.as_deref(),
        workspace,
        templates::Delivery::Watched,
    );
    match instruction_args(settings, agent, &instructions) {
        Some(instruction_args) => (prompt, instruction_args.into_iter().chain(args).collect()),
        None => {
            let prompt = match prompt {
                Some(prompt) if !prompt.is_empty() => format!("{instructions}\n\n{prompt}"),
                _ => instructions,
            };
            (Some(prompt), args)
        }
    }
}

/// The arguments that give `agent` instructions apart from its first
/// message; `None` for Happy's Codex mode, which takes no arguments.
pub(super) fn instruction_args(
    settings: &Effective,
    agent: Agent,
    instructions: &str,
) -> Option<Vec<OsString>> {
    settings
        .launch(agent.as_str())
        .arguments
        .instruction_args(instructions)
        .filter(|_| happy::accepts_prompt(agent))
}

/// Whether Happy's daemon has recorded itself, warning when it has not, since
/// its sessions appear in the app only once it runs.
pub(super) fn warn_without_daemon(ctx: &Context) -> bool {
    let daemon_state = happy::daemon_state_path(&ctx.paths.home);
    let recorded = daemon_state.is_file();
    if !recorded {
        eprintln!(
            "warning: Happy daemon state not found at {}; the session will not appear in the Happy app until `happy daemon start` runs",
            daemon_state.display()
        );
    }
    recorded
}

/// Keep `prompt` beside the session's other `files`, for the user to send
/// when Happy cannot deliver it.
fn save_prompt(files: &Path, prompt: &str) -> Result<PathBuf> {
    let path = files.with_extension("prompt.md");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&path, prompt).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// How a session's first message reaches it. Happy's Codex mode has no prompt
/// argument: the prompt is kept in a file and, when this machine is logged in
/// to Happy, delivered through Happy's server to a session Shoal creates for
/// the agent to attach to.
pub(super) struct Opening {
    pub(super) prompt_file: Option<PathBuf>,
    pub(super) seeded: Option<happy::client::Seeded>,
}

impl Opening {
    /// Keep the prompt beside the session's other `files` and seed its
    /// session, for an agent that cannot take it as an argument.
    pub(super) async fn prepare(
        ctx: &Context,
        agent: Agent,
        directory: &Path,
        branch: Option<&str>,
        files: &Path,
        prompt: Option<&str>,
    ) -> Result<Self> {
        let Some(prompt) = prompt.filter(|_| !happy::accepts_prompt(agent)) else {
            return Ok(Self {
                prompt_file: None,
                seeded: None,
            });
        };
        let path = save_prompt(files, prompt)?;
        let seeded = match happy::client::seed(&ctx.paths, directory, branch, agent).await {
            Ok(seeded) => Some(seeded),
            Err(error) => {
                eprintln!(
                    "warning: cannot deliver the prompt through Happy ({error:#}); send the prompt saved at {} to the session yourself",
                    path.display()
                );
                None
            }
        };
        Ok(Self {
            prompt_file: Some(path),
            seeded,
        })
    }

    /// The variables that attach the session to the seeded one.
    pub(super) fn env(&self) -> Vec<(&'static str, String)> {
        self.seeded
            .as_ref()
            .map(|seeded| seeded.session.env())
            .unwrap_or_default()
    }

    /// Nothing will ever attach to the seeded session; do not leave it in the app.
    pub(super) async fn discard(self) {
        if let Some(seeded) = self.seeded {
            seeded.discard().await;
        }
    }

    /// Send `prompt` to the started session, returning whether it arrived. A
    /// prompt passed as an argument is delivered by the launch itself.
    pub(super) async fn deliver(&self, prompt: Option<&str>) -> bool {
        let Some(prompt) = prompt else {
            return false;
        };
        let (Some(seeded), Some(path)) = (&self.seeded, &self.prompt_file) else {
            return self.prompt_file.is_none();
        };
        match seeded
            .deliver(prompt, std::time::Duration::from_secs(90))
            .await
        {
            Ok(()) => true,
            Err(error) => {
                eprintln!(
                    "warning: prompt not delivered ({error:#}); send the prompt saved at {} to the session yourself",
                    path.display()
                );
                false
            }
        }
    }

    /// The line reporting where a saved prompt went.
    pub(super) fn delivery(&self, delivered: bool) -> &'static str {
        match (&self.prompt_file, delivered) {
            (Some(_), true) => "\nPrompt delivered to the session",
            (Some(_), false) => "\nPrompt saved for you to send from the app",
            (None, _) => "",
        }
    }
}
