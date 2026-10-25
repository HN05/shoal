//! Detached Happy sessions and their tool-specific prompt delivery.
use std::{
    ffi::OsString,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Stdio},
};

use anyhow::{Context as _, Result};
use futures_util::future::BoxFuture;
use serde_json::json;

use super::{ChatLaunch, Integration, Launch, trust::TrustFile};
use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    config::{Effective, templates},
    execution,
    happy::{self, Agent},
    model::{Chat, Repository, Workspace},
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

    fn unavailable(&self, settings: &Effective) -> Option<&'static str> {
        (!settings.happy.enabled).then_some(
            "Happy sessions are off; set happy.enabled = true in the repository or global config",
        )
    }

    fn launches_itself(&self, _settings: &Effective, _prompted: bool, _tracked: bool) -> bool {
        true
    }

    fn chat<'a>(
        &'a self,
        ctx: &'a Context,
        chat: &'a ChatLaunch,
    ) -> BoxFuture<'a, Result<Option<i32>>> {
        Box::pin(async move { start_chat(ctx, self, chat).await.map(Some) })
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

/// The agents `shoal happy <agent>` can start: each `happy-<agent>` offered
/// and installed.
pub(in crate::cli) fn shortcut_agents<'a>(installed: &super::Installed<'a>) -> Vec<&'a str> {
    installed
        .agents
        .iter()
        .filter(|name| installed.agent(name))
        .filter_map(|name| name.strip_prefix("happy-"))
        .collect()
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
fn instruction_args(
    settings: &Effective,
    agent: Agent,
    instructions: &str,
) -> Option<Vec<OsString>> {
    settings
        .arguments(agent.as_str())
        .instruction_args(instructions)
        .filter(|_| happy::accepts_prompt(agent))
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
        trust
            .trust_tracked(ctx, workspace, &Default::default())
            .await;
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

/// Start a chat as a Happy session in the repository's checkout, detached as
/// workspace sessions are, since the user continues it in the Happy app. It is
/// no tracked execution: the chat lasts while the session's process runs.
async fn start_chat(ctx: &Context, happy: &Happy, chat: &ChatLaunch) -> Result<i32> {
    let agent = happy.0;
    let name = format!("happy-{}", agent.as_str());
    let ChatLaunch {
        repository,
        settings,
        instructions,
        opening,
        args,
    } = chat;
    super::ensure_available(&name, settings)?;
    let (prompt, args) = match instruction_args(settings, agent, instructions) {
        Some(instruction_args) => (
            opening.clone(),
            instruction_args.into_iter().chain(args.clone()).collect(),
        ),
        None => (
            templates::chat_first_message(instructions, opening.as_deref()),
            args.clone(),
        ),
    };
    if let Some(trust) = happy.trust_file() {
        trust.trust_checkout(ctx, repository, &Default::default());
    }
    let daemon_recorded = warn_without_daemon(ctx);
    // This process holds the chat until the session runs.
    let record = client::start_chat(&ctx.paths, repository, &name).await?;
    let files = ctx.paths.chat_files(&record);
    let opening = Opening::prepare(
        ctx,
        agent,
        &repository.path,
        None,
        &files,
        prompt.as_deref(),
    )
    .await?;
    let command = happy::command(agent, prompt.as_deref(), args);
    let log = files.with_extension("log");
    let started = spawn_session(ctx, repository, &record, &log, command, &opening.env());
    let handed_over = match started {
        Ok(session) => hand_over(ctx, &record, session).await,
        Err(error) => Err(error),
    };
    let record = match handed_over {
        Ok(moved) => moved,
        Err(error) => {
            opening.discard().await;
            return Err(error);
        }
    };
    let prompt_delivered = opening.deliver(prompt.as_deref()).await;
    ctx.emit(
        &format!(
            "Started Happy {} chat {} for {} (pid {}); open it in the Happy app\nOutput: {}{}",
            agent.as_str(),
            record.number,
            crate::repository::name(repository),
            record.pid,
            log.display(),
            opening.delivery(prompt_delivered),
        ),
        json!({
            "chat": record,
            "log": log,
            "prompt_file": opening.prompt_file,
            "prompt_delivered": prompt_delivered,
            "happy_session_id": opening.seeded.as_ref().map(|seeded| seeded.session.id.clone()),
            "happy_daemon_recorded": daemon_recorded,
        }),
    )?;
    Ok(0)
}

/// Hand `chat` to `session`'s process, stopping the session when the daemon
/// cannot record it, since the chat would end with this process.
async fn hand_over(ctx: &Context, chat: &Chat, session: Child) -> Result<Chat> {
    let moved = client::move_chat(&ctx.paths, chat, session.id()).await;
    if moved.is_err() {
        stop_session(session);
    }
    moved
}

/// Stop a session started by [`spawn_session`] with the agent it started:
/// the session leads its own process group.
fn stop_session(mut session: Child) {
    // SAFETY: killpg takes no pointers. The leader is not reaped yet, so its
    // ID still names this group alone.
    unsafe { libc::killpg(session.id() as libc::pid_t, libc::SIGKILL) };
    let _ = session.wait();
}

/// Start `command` in its own session at the checkout with the chat's
/// identity and `env`, writing to `log`.
fn spawn_session(
    ctx: &Context,
    repository: &Repository,
    chat: &Chat,
    log: &Path,
    command: Vec<OsString>,
    env: &[(&str, String)],
) -> Result<Child> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let output = std::fs::File::create(log).with_context(|| format!("create {}", log.display()))?;
    let mut session = std::process::Command::new(&command[0]);
    session
        .args(&command[1..])
        .current_dir(&repository.path)
        .envs(crate::env::chat_environment(chat, &ctx.paths))
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output);
    // A launch from inside another Happy session must not attach to it.
    for name in happy::client::RECONNECT_ENV {
        session.env_remove(name);
    }
    session.envs(env.iter().map(|(name, value)| (*name, value)));
    // SAFETY: setsid detaches the forked child without allocating, before exec.
    unsafe {
        session.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    session.spawn().context("start the Happy session")
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

#[cfg(test)]
mod tests {
    use std::{
        os::unix::process::CommandExt,
        time::{Duration, Instant},
    };

    use super::*;

    #[test]
    fn stopping_a_session_stops_the_agent_it_started() {
        let directory = tempfile::tempdir().unwrap();
        let agent_pid = directory.path().join("agent.pid");
        let session = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 600 & echo $! > \"$1\"; wait")
            .arg("session")
            .arg(&agent_pid)
            .process_group(0)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let agent = loop {
            if let Ok(pid) = std::fs::read_to_string(&agent_pid)
                && let Ok(pid) = pid.trim().parse::<u32>()
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "the session never started its agent"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let agent = crate::process::identity::capture(agent)
            .unwrap()
            .expect("the agent runs");
        stop_session(session);
        // An orphaned agent may stay a zombie where nothing reaps it, which
        // the identity check counts as ended.
        while crate::process::identity::capture(agent.pid)
            .unwrap()
            .as_ref()
            == Some(&agent)
        {
            assert!(Instant::now() < deadline, "the agent outlived its session");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
