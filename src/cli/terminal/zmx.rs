//! zmx sessions keep terminal agents running after their terminal closes. zmx
//! owns the pty, passes output through unchanged while attached, and restores
//! the screen on attach; Shoal names each session after its workspace and
//! labels it with the workspace ID so `shoal attach` can find it. Repository
//! chats are labelled with the repository ID instead.
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    io::IsTerminal,
    path::PathBuf,
    process::ExitStatus,
};

use anyhow::{Context as _, Result, bail};
use futures_util::future::BoxFuture;
use tokio::process::Command;

use super::{DemoWorker, Host, Place, Placement, Session};
use crate::{
    cli::{
        agents::ensure_program,
        commands::{
            issues::Issue,
            workspaces::{AddPlan, execute_add},
        },
        context::Context,
        internal::{self, Worker, internal_command},
    },
    config::Effective,
    env,
    model::{Repository, Workspace},
    repository,
    subprocess::Run,
    tools::Tool,
};

/// zmx sets this inside a session; a launch there runs in place.
const SESSION_VAR: &str = "ZMX_SESSION";
const WORKSPACE_LABEL: &str = "shoal.workspace";
const CHAT_LABEL: &str = "shoal.chat";
const AGENT_LABEL: &str = "shoal.agent";
/// zmx names its socket after the session, and `sun_path` is short on macOS.
const MAX_NAME_LEN: usize = 32;

const NAME: &str = "zmx";

/// zmx keeps agents in sessions that outlive the caller's terminal, outside
/// Herdr panes.
pub(super) struct Zmx;

impl Host for Zmx {
    fn name(&self) -> &'static str {
        NAME
    }

    fn serves(&self, _ctx: &Context, _settings: &Effective) -> bool {
        on_terminal()
    }

    fn ensure_installed(&self) -> Result<()> {
        ensure_program("background launch", Tool::Zmx.program())
    }

    /// A detached session needs no terminal, so it takes any queued start.
    fn takes_queued(&self, _settings: &Effective) -> bool {
        true
    }

    /// A focused start stays here, where its agent runs in a session this
    /// terminal attaches to.
    fn start_add<'a>(
        &'a self,
        ctx: &'a Context,
        _settings: &'a Effective,
        plan: &'a mut AddPlan,
        _issue: Option<&'a Issue>,
        name: &'a str,
        placement: Placement,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            if placement == Placement::Focus {
                return Ok(false);
            }
            start_add(ctx, plan, name).await?;
            Ok(true)
        })
    }

    fn run_in_background<'a>(
        &'a self,
        ctx: &'a Context,
        _settings: &'a Effective,
        workspace: &'a Workspace,
        args: &'a [OsString],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            run_in_background(ctx, workspace, args).await?;
            Ok(true)
        })
    }

    fn run_agent<'a>(
        &'a self,
        ctx: &'a Context,
        workspace: &'a Workspace,
        agent: &'a str,
        records: &'a [PathBuf],
        command: &'a [OsString],
    ) -> BoxFuture<'a, Result<Option<i32>>> {
        Box::pin(async move { run(ctx, workspace, agent, records, command).await.map(Some) })
    }

    fn start_chat<'a>(
        &'a self,
        ctx: &'a Context,
        _settings: &'a Effective,
        repository: &'a Repository,
        agent: &'a str,
        _label: &'a str,
        args: &'a [OsString],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            run_chat(ctx, repository, agent, args).await?;
            Ok(true)
        })
    }

    fn background_places(&self) -> &'static str {
        "detached zmx sessions"
    }

    fn background_note(&self, name: &str) -> String {
        format!("Started {name} in a detached zmx session; attach with `shoal attach {name}`")
    }

    fn prompt_session(&self) -> Option<OsString> {
        std::env::var_os(SESSION_VAR)
    }

    fn submit_prompt<'a>(&'a self, session: &'a OsStr, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(submit_prompt(session, text))
    }

    fn label_current<'a>(
        &'a self,
        workspace_id: &'a str,
        agent: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(label_current(workspace_id, agent))
    }

    fn keeps_agents_in_sessions(&self) -> bool {
        on_terminal()
    }

    fn launcher_program(&self) -> Option<&'static str> {
        on_terminal().then(|| Tool::Zmx.program())
    }

    fn sessions<'a>(&'a self, workspace_id: &'a str) -> BoxFuture<'a, Result<Vec<Session>>> {
        Box::pin(sessions(workspace_id))
    }

    fn session_workspaces(&self) -> BoxFuture<'_, Result<HashSet<String>>> {
        Box::pin(session_workspaces())
    }

    fn attach<'a>(
        &'a self,
        workspace: &'a Workspace,
        session: &'a str,
    ) -> BoxFuture<'a, Result<i32>> {
        Box::pin(attach(workspace, session))
    }

    fn start_demo<'a>(
        &'a self,
        _ctx: &'a Context,
        workspace: &'a Workspace,
        name: &'a str,
        worker: &'a DemoWorker<'a>,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move { start_demo(workspace, name, worker).await.map(Some) })
    }

    fn open_demo<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(open_demo(place))
    }

    fn demo_screen<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(demo_screen(place))
    }

    fn send_demo_keys<'a>(&'a self, place: &'a str, keys: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(send_keys(place, keys))
    }
}

/// The detached session the zmx worker runs in: it adds the workspace there
/// in place of the caller's shell, and ends with its agent, leaving no shell.
struct DetachedSession;

impl Place for DetachedSession {
    fn host_workspace<'a>(
        &'a self,
        _ctx: &'a Context,
        workspace: &'a Workspace,
        agent: Option<&'a str>,
        _label: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if let Err(error) = label_current(&workspace.id, agent.unwrap_or_default()).await {
                eprintln!("warning: {error:#}");
            }
            Ok(())
        })
    }

    fn keeps_shell(&self) -> bool {
        false
    }
}

/// Whether this process could attach a new session: a terminal outside zmx.
fn on_terminal() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::env::var_os(SESSION_VAR).is_none()
}

/// Start `command` as `agent`'s tracked execution in a new session and attach
/// this terminal to it. zmx does not report the agent's exit status, so a
/// session that ends or detaches returns 0; the daemon still records the exit.
async fn run(
    ctx: &Context,
    workspace: &Workspace,
    agent: &str,
    records: &[PathBuf],
    command: &[OsString],
) -> Result<i32> {
    // Launches check zmx before any work; resumed agents reach it here.
    ensure_program(agent, Tool::Zmx.program())?;
    let taken = list().await?.into_iter().map(|session| session.0).collect();
    let name = free_name(&workspace.name, &taken);
    let worker = internal_command(
        &ctx.paths,
        false,
        Worker::Session {
            workspace: &workspace.id,
            agent,
            records,
            command,
        },
    )?;
    let mut zmx = Command::new(Tool::Zmx.program());
    zmx.arg("attach")
        .arg("--labels")
        .arg(labels(WORKSPACE_LABEL, &workspace.id, agent))
        .arg(&name)
        .args(&worker)
        .env_remove(env::SHELL_DIRECTIVE);
    if let Some(entry) = env::queue_entry() {
        zmx.env(env::QUEUE_ENTRY, entry);
    }
    let status = zmx.status().await.context("run zmx")?;
    finish(workspace, &name, status).await
}

/// Run Shoal with `args` for a chat in a new session at the repository's
/// checkout and attach this terminal to it. The session's Shoal records the
/// chat and becomes its agent, as it would in place.
async fn run_chat(
    ctx: &Context,
    repository: &Repository,
    agent: &str,
    args: &[OsString],
) -> Result<i32> {
    let taken = list().await?.into_iter().map(|session| session.0).collect();
    let name = free_name(&format!("{}-chat", repository::name(repository)), &taken);
    let mut argv = internal::session_argv(&ctx.paths)?;
    argv.extend_from_slice(args);
    let status = Command::new(Tool::Zmx.program())
        .arg("attach")
        .arg("--labels")
        .arg(labels(CHAT_LABEL, &repository.id, agent))
        .arg(&name)
        .args(&argv)
        .current_dir(&repository.path)
        .env_remove(env::SHELL_DIRECTIVE)
        .status()
        .await
        .context("run zmx")?;
    crate::execution::reset_input_modes();
    if !status.success() {
        bail!("zmx exited with {status}");
    }
    if list().await?.iter().any(|(session, _)| *session == name) {
        eprintln!("shoal: detached from {name}; run `zmx attach {name}` to return");
    }
    Ok(0)
}

/// The detached session's worker: add the workspace its launch planned, in
/// the session in place of the caller's shell.
pub async fn worker(mut ctx: Context, payload: &str) -> Result<i32> {
    let plan = serde_json::from_str(payload).context("invalid workspace plan")?;
    ctx.place = Some(Box::new(DetachedSession));
    execute_add(&ctx, plan).await
}

/// Run `plan` in a new detached session named after its planned workspace
/// `name`. The session's worker reads the plan from its environment, as a
/// Herdr tab's does.
async fn start_add(ctx: &Context, plan: &AddPlan, name: &str) -> Result<()> {
    let argv = internal_command(&ctx.paths, false, Worker::Add)?;
    let plan = serde_json::to_string(plan)?;
    start_detached(name, &argv, &[(env::ADD_PLAN, &plan)]).await
}

/// Run Shoal with `args` for `workspace` in a new detached session named
/// after it.
async fn run_in_background(ctx: &Context, workspace: &Workspace, args: &[OsString]) -> Result<()> {
    let mut argv = internal::shoal_argv(&ctx.paths)?;
    argv.extend_from_slice(args);
    start_detached(&workspace.name, &argv, &[]).await
}

/// Run `argv` in a new detached session named after `name`, as if typed at
/// its shell, since zmx gives commands it runs itself no terminal input. That
/// shell is zmx's bash, so the session ends once `argv` succeeds, which it does
/// once its agent or workspace shell has run, and otherwise becomes the user's
/// login shell to show why it failed. `env` reaches that shell, inert there.
async fn start_detached(name: &str, argv: &[OsString], env: &[(&str, &str)]) -> Result<()> {
    ensure_program("background launch", Tool::Zmx.program())?;
    let taken = list().await?.into_iter().map(|session| session.0).collect();
    run_in_new_session(&free_name(name, &taken), argv, env, &[]).await
}

/// Start a demo in a new detached session named after the workspace and the
/// demo, whose worker ends the session once the demo stops.
async fn start_demo(workspace: &Workspace, name: &str, worker: &DemoWorker<'_>) -> Result<String> {
    ensure_program("demo", Tool::Zmx.program())?;
    let taken = list().await?.into_iter().map(|session| session.0).collect();
    let session = free_name(&format!("{}-{name}", workspace.name), &taken);
    run_in_new_session(
        &session,
        &worker(&session)?,
        &[],
        super::NOT_INHERITED_BY_DEMOS,
    )
    .await?;
    Ok(session)
}

/// Create the session `name` and run `argv` there as `start_detached`
/// describes, without the variables `clear` names.
async fn run_in_new_session(
    name: &str,
    argv: &[OsString],
    env: &[(&str, &str)],
    clear: &[&str],
) -> Result<()> {
    let words = argv
        .iter()
        .map(|arg| arg.to_str().context("Shoal command path is not UTF-8"))
        .collect::<Result<Vec<_>>>()?;
    let mut create = Command::new(Tool::Zmx.program());
    create
        .args(["run", name, "-d", "true"])
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::null());
    for name in clear {
        create.env_remove(name);
    }
    Run::new(create)
        .checked()
        .await
        .context("create a zmx session")?;
    let mut send = Command::new(Tool::Zmx.program());
    // A session zmx started elsewhere may carry them anyway.
    let unset = if clear.is_empty() {
        String::new()
    } else {
        format!("unset {}; ", clear.join(" "))
    };
    send.args(["send", name]).arg(format!(
        "{unset}{}=1 {} && exit; exec \"$SHELL\" -l\r",
        env::ENDS_SESSION,
        crate::shell::quote(&words)
    ));
    Run::new(send)
        .checked()
        .await
        .context("start Shoal in its zmx session")?;
    Ok(())
}

/// Attach this terminal to the demo's session until the user detaches.
async fn open_demo(session: &str) -> Result<()> {
    ensure_program("demo", Tool::Zmx.program())?;
    let status = Command::new(Tool::Zmx.program())
        .args(["attach", session])
        .status()
        .await
        .context("run zmx")?;
    crate::execution::reset_input_modes();
    if !status.success() {
        bail!("zmx exited with {status}");
    }
    Ok(())
}

/// The session's scrollback as plain text.
async fn demo_screen(session: &str) -> Result<String> {
    let mut history = Command::new(Tool::Zmx.program());
    history.args(["history", session]);
    let output = Run::new(history)
        .checked()
        .await
        .context("read the demo's screen")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Type `keys` into the session as they are, control characters included.
async fn send_keys(session: &str, keys: &str) -> Result<()> {
    let mut send = Command::new(Tool::Zmx.program());
    send.args(["send", session, keys])
        .stdin(std::process::Stdio::null());
    Run::new(send)
        .checked()
        .await
        .context("type into the demo's session")?;
    Ok(())
}

/// Submit `text` as a prompt in `session`, as if pasted and sent: bracketed
/// paste keeps it one prompt, and Enter in the same write submits it rather
/// than ending the paste.
async fn submit_prompt(session: &OsStr, text: &str) -> Result<()> {
    let mut send = Command::new(Tool::Zmx.program());
    send.arg("send")
        .arg(session)
        .arg(format!("\x1b[200~{text}\x1b[201~\r"))
        .stdin(std::process::Stdio::null());
    Run::new(send)
        .checked()
        .await
        .context("type into the zmx session")?;
    Ok(())
}

/// Label the zmx session this process runs in, when it runs in one, so
/// `shoal attach` finds the workspace's agent there.
async fn label_current(workspace_id: &str, agent: &str) -> Result<()> {
    let Some(session) = std::env::var_os(SESSION_VAR) else {
        return Ok(());
    };
    let mut set = Command::new(Tool::Zmx.program());
    set.arg("set")
        .arg(session)
        .args(labels(WORKSPACE_LABEL, workspace_id, agent).split(' '));
    Run::new(set)
        .checked()
        .await
        .context("label the zmx session")?;
    Ok(())
}

/// Attach this terminal to `session`, reporting a detach as `run` does.
async fn attach(workspace: &Workspace, session: &str) -> Result<i32> {
    ensure_program("attach", Tool::Zmx.program())?;
    let status = Command::new(Tool::Zmx.program())
        .args(["attach", session])
        .status()
        .await
        .context("run zmx")?;
    finish(workspace, session, status).await
}

async fn finish(workspace: &Workspace, name: &str, status: ExitStatus) -> Result<i32> {
    crate::execution::reset_input_modes();
    if !status.success() {
        bail!("zmx exited with {status}");
    }
    if sessions(&workspace.id)
        .await?
        .iter()
        .any(|session| session.name == name)
    {
        eprintln!(
            "shoal: detached from {name}; run `shoal attach {}` to return",
            workspace.name
        );
    }
    Ok(0)
}

/// The workspace's running sessions, oldest name first.
async fn sessions(workspace_id: &str) -> Result<Vec<Session>> {
    let mut sessions: Vec<_> = list()
        .await?
        .into_iter()
        .filter(|(_, fields)| field(fields, WORKSPACE_LABEL) == Some(workspace_id))
        .map(|(name, fields)| Session {
            agent: field(&fields, AGENT_LABEL).map(str::to_owned),
            clients: field(&fields, "clients")
                .and_then(|clients| clients.parse().ok())
                .unwrap_or(0),
            name,
            host: NAME,
        })
        .collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sessions)
}

/// The IDs of workspaces with at least one agent session.
async fn session_workspaces() -> Result<HashSet<String>> {
    Ok(list()
        .await?
        .into_iter()
        .filter_map(|(_, fields)| field(&fields, WORKSPACE_LABEL).map(str::to_owned))
        .collect())
}

type Fields = Vec<(String, String)>;

/// Every zmx session with its fields and labels.
async fn list() -> Result<Vec<(String, Fields)>> {
    let mut command = Command::new(Tool::Zmx.program());
    command.arg("list");
    let output = Run::new(command)
        .checked()
        .await
        .context("list zmx sessions")?;
    Ok(parse_list(&String::from_utf8_lossy(&output.stdout)))
}

/// `zmx list` prints one session per line after a two-column marker, as
/// tab-separated `key=value` fields followed by its labels.
fn parse_list(output: &str) -> Vec<(String, Fields)> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Fields = line[line.find("name=")?..]
                .split('\t')
                .filter_map(|field| field.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect();
            let name = field(&fields, "name")?.to_owned();
            Some((name, fields))
        })
        .collect()
}

fn field<'a>(fields: &'a Fields, key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// zmx separates labels with spaces, so an agent name containing one is left out.
fn labels(owner: &str, id: &str, agent: &str) -> String {
    let mut labels = format!("{owner}={id}");
    if !agent.is_empty() && !agent.contains(char::is_whitespace) {
        labels.push_str(&format!(" {AGENT_LABEL}={agent}"));
    }
    labels
}

/// The workspace name, shortened to fit a socket path, then suffixed `-2`,
/// `-3`, ... past sessions that already use it.
fn free_name(workspace: &str, taken: &HashSet<String>) -> String {
    let base: String = workspace.chars().take(MAX_NAME_LEN).collect();
    std::iter::once(base.clone())
        .chain((2..).map(|n| format!("{base}-{n}")))
        .find(|name| !taken.contains(name))
        .expect("an unused suffix exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_parses_names_fields_and_labels_after_the_marker() {
        let output = "  name=fix-login\tpid=1\tclients=0\tcreated=1\tcwd=file://host/a\t\
            cmd=shoal internal session\tshoal.agent=claude\tshoal.workspace=w1\n\
            \u{2192} name=other\tpid=2\tclients=1\tcmd=sh -c 'x=y'\n\
            no sessions found in /tmp/zmx-501\n";
        let sessions = parse_list(output);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].0, "fix-login");
        assert_eq!(field(&sessions[0].1, "shoal.workspace"), Some("w1"));
        assert_eq!(field(&sessions[0].1, "shoal.agent"), Some("claude"));
        assert_eq!(sessions[1].0, "other");
        assert_eq!(field(&sessions[1].1, "clients"), Some("1"));
        assert_eq!(field(&sessions[1].1, "cmd"), Some("sh -c 'x=y'"));
    }

    #[test]
    fn names_fit_the_socket_path_and_skip_taken_ones() {
        let taken = HashSet::from(["fix".to_owned(), "fix-2".to_owned()]);
        assert_eq!(free_name("fix", &taken), "fix-3");
        assert_eq!(free_name("other", &taken), "other");
        assert_eq!(free_name(&"x".repeat(40), &taken), "x".repeat(MAX_NAME_LEN));
    }

    #[test]
    fn labels_leave_out_agent_names_zmx_cannot_hold() {
        assert_eq!(
            labels(WORKSPACE_LABEL, "w1", "claude"),
            "shoal.workspace=w1 shoal.agent=claude"
        );
        assert_eq!(
            labels(WORKSPACE_LABEL, "w1", "my agent"),
            "shoal.workspace=w1"
        );
        assert_eq!(
            labels(CHAT_LABEL, "r1", "codex"),
            "shoal.chat=r1 shoal.agent=codex"
        );
    }
}
