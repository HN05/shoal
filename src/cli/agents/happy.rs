//! Detached Happy sessions and their tool-specific prompt delivery.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use futures_util::future::BoxFuture;
use serde_json::json;

use super::{Integration, Launch, trust::TrustFile};
use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    config::{Effective, templates},
    execution,
    happy::{self, Agent},
    model::Workspace,
    protocol::ConfigTarget,
};

/// `happy-<agent>`: the agent in a detached Happy session.
pub(super) struct Happy(Agent);

pub(super) const CLAUDE: Happy = Happy(Agent::Claude);
pub(super) const CODEX: Happy = Happy(Agent::Codex);

impl Happy {
    /// The integration of the agent Happy runs.
    fn inner(&self) -> &'static dyn Integration {
        super::integration(self.0.as_str())
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

    /// A Happy session starts detached, not in place of Shoal.
    fn chats(&self) -> bool {
        false
    }

    fn unavailable(&self, settings: &Effective) -> Option<&'static str> {
        (!settings.happy.enabled).then_some(
            "Happy sessions are off; set happy.enabled = true in the repository or global config",
        )
    }

    fn launch<'a>(
        &'a self,
        ctx: &'a Context,
        launch: &'a mut Launch,
    ) -> BoxFuture<'a, Result<Option<i32>>> {
        Box::pin(async move {
            let prompt = launch.prompt.take();
            let args = std::mem::take(&mut launch.args);
            start(ctx, self, &launch.workspace, &launch.settings, prompt, args)
                .await
                .map(Some)
        })
    }
}

/// `shoal happy <agent>`.
pub(in crate::cli) async fn shortcut(
    ctx: &Context,
    agent: Agent,
    workspace: Option<String>,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    start(ctx, &Happy(agent), &workspace, &settings, prompt, args).await
}

/// The first message and arguments of a session of `agent`, which runs
/// without Shoal's hooks. An agent that takes instructions apart from its
/// prompt gets them as arguments; Happy's Codex mode takes no arguments, so
/// they lead its first message.
fn session_input(
    settings: &Effective,
    workspace: &Workspace,
    agent: Agent,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> (Option<String>, Vec<OsString>) {
    let instructions = templates::instructions(
        settings.agent_template.as_deref(),
        workspace,
        templates::Delivery::Watched,
    );
    let instruction_args = settings
        .arguments(agent.as_str())
        .instruction_args(&instructions)
        .filter(|_| happy::accepts_prompt(agent));
    match instruction_args {
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

/// Start a Happy session the way Happy's own daemon does, so it registers with
/// that daemon and appears in the app, but detached from this terminal and
/// tracked like any other workspace command. The CLI returns once the launch
/// is recorded; the session's output goes to a log under Shoal's state.
async fn start(
    ctx: &Context,
    happy: &Happy,
    workspace: &Workspace,
    settings: &Effective,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let agent = happy.0;
    super::ensure_available(&format!("happy-{}", agent.as_str()), settings)?;
    let (prompt, args) = session_input(settings, workspace, agent, prompt, args);
    if let Some(trust) = happy.trust_file() {
        trust.trust_tracked(ctx, workspace).await;
    }
    let daemon_recorded = warn_without_daemon(ctx);
    let stamp_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let files = ctx
        .paths
        .workspace_state(&workspace.id)
        .join(format!("happy-{}-{stamp_millis}", agent.as_str()));
    let opening = Opening::prepare(
        ctx,
        agent,
        &workspace.path,
        Some(&workspace.branch),
        &files,
        prompt.as_deref(),
    )
    .await?;
    let command = happy::command(agent, prompt.as_deref(), args);
    let launch = match execution::launch_detached(
        &ctx.paths,
        workspace,
        files.with_extension("log"),
        command,
        &happy::client::RECONNECT_ENV,
        &opening.env(),
        Some(&format!("happy {}", agent.as_str())),
    )
    .await
    {
        Ok(launch) => launch,
        Err(error) => {
            opening.discard().await;
            return Err(error);
        }
    };
    let prompt_delivered = opening.deliver(prompt.as_deref()).await;
    ctx.emit(
        &format!(
            "Started Happy {} session in {} (execution {}, pid {})\nOutput: {}{}",
            agent.as_str(),
            workspace.name,
            launch.execution_id,
            launch.pid,
            launch.log.display(),
            opening.delivery(prompt_delivered),
        ),
        json!({
            "workspace": workspace,
            "agent": agent,
            "execution_id": launch.execution_id,
            "pid": launch.pid,
            "log": launch.log,
            "prompt_file": opening.prompt_file,
            "prompt_delivered": prompt_delivered,
            "happy_session_id": opening.seeded.as_ref().map(|seeded| seeded.session.id.clone()),
            "happy_daemon_recorded": daemon_recorded,
        }),
    )?;
    Ok(0)
}

/// Whether Happy's daemon has recorded itself, warning when it has not, since
/// its sessions appear in the app only once it runs.
fn warn_without_daemon(ctx: &Context) -> bool {
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

/// How a session's first message reaches it. Happy's Codex mode has no prompt
/// argument: the prompt is kept in a file and, when this machine is logged in
/// to Happy, delivered through Happy's server to a session Shoal creates for
/// the agent to attach to.
struct Opening {
    prompt_file: Option<PathBuf>,
    seeded: Option<happy::client::Seeded>,
}

impl Opening {
    /// Keep the prompt beside the session's other `files` and seed its
    /// session, for an agent that cannot take it as an argument.
    async fn prepare(
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
        let path = files.with_extension("prompt.md");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&path, prompt).with_context(|| format!("write {}", path.display()))?;
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
    fn env(&self) -> Vec<(&'static str, String)> {
        self.seeded
            .as_ref()
            .map(|seeded| seeded.session.env())
            .unwrap_or_default()
    }

    /// Nothing will ever attach to the seeded session; do not leave it in the app.
    async fn discard(self) {
        if let Some(seeded) = self.seeded {
            seeded.discard().await;
        }
    }

    /// Send `prompt` to the started session, returning whether it arrived. A
    /// prompt passed as an argument is delivered by the launch itself.
    async fn deliver(&self, prompt: Option<&str>) -> bool {
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
    fn delivery(&self, delivered: bool) -> &'static str {
        match (&self.prompt_file, delivered) {
            (Some(_), true) => "\nPrompt delivered to the session",
            (Some(_), false) => "\nPrompt saved for you to send from the app",
            (None, _) => "",
        }
    }
}
