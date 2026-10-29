//! zmx sessions keep terminal agents running after their terminal closes. zmx
//! owns the pty, passes output through unchanged while attached, and restores
//! the screen on attach. Shoal names each session after its workspace or chat,
//! and `shoal attach` finds it where the daemon recorded the agent or chat
//! running; labels with the workspace's or chat's ID only decide which owner's
//! cleanup closes it.
use std::{
    collections::{BTreeMap, HashSet},
    ffi::{OsStr, OsString},
    io::IsTerminal,
    path::Path,
    process::ExitStatus,
};

use anyhow::{Context as _, Result, bail};
use futures_util::future::BoxFuture;
use tokio::process::Command;

use super::{
    ChatStart, Demo, Host, Owner, OwnerKind, PlaceState, Session, Start, Watched,
    host::{Closes, Demos, Places, Prompts, Sessions, Watches},
};
use crate::{
    cli::agents::ensure_program,
    env,
    sessions::{
        HostName, Origin,
        zmx::{CHAT_LABEL, CREATED, Fields, WORKSPACE_LABEL, field, kill, list},
    },
    subprocess::Run,
    tools::Tool,
};

/// zmx sets this inside a session; a launch there runs in place.
const SESSION_VAR: &str = "ZMX_SESSION";
/// zmx names its socket after the session, and `sun_path` is short on macOS.
const MAX_NAME_LEN: usize = 32;

/// zmx keeps agents in sessions that outlive the caller's terminal, outside
/// Herdr panes.
pub(super) struct Zmx;

impl Host for Zmx {
    fn name(&self) -> HostName {
        HostName::Zmx
    }

    fn serves(&self) -> bool {
        on_terminal()
    }

    fn ensure_installed(&self) -> Result<()> {
        ensure_program("background launch", Tool::Zmx.program())
    }

    fn places(&self) -> Option<&dyn Places> {
        Some(self)
    }

    fn watches(&self) -> Option<&dyn Watches> {
        Some(self)
    }

    fn closes(&self) -> Option<&dyn Closes> {
        Some(self)
    }

    fn sessions(&self) -> Option<&dyn Sessions> {
        Some(self)
    }

    fn prompts(&self) -> Option<&dyn Prompts> {
        Some(self)
    }

    fn demos(&self) -> Option<&dyn Demos> {
        Some(self)
    }
}

impl Places for Zmx {
    /// A detached session needs no terminal, so it takes any queued start.
    /// A session is no group a later place joins, so a start queued from
    /// one goes to the default host first.
    fn takes_queued(&self, origin: Option<&Origin>) -> bool {
        origin.is_none_or(|origin| origin.host != HostName::Zmx)
    }

    /// A focused start stays here, where its agent runs in a session this
    /// terminal attaches to.
    fn start_add<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            if start.focus {
                return Ok(false);
            }
            start_detached(start).await?;
            Ok(true)
        })
    }

    fn run_in_background<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<()>> {
        Box::pin(start_detached(start))
    }

    fn start_chat<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<ChatStart>> {
        Box::pin(async move { run_chat(start).await.map(ChatStart::Session) })
    }

    fn background_places(&self) -> &'static str {
        "detached zmx sessions"
    }

    fn background_note(&self, name: &str) -> String {
        format!("Started {name} in a detached zmx session; attach with `shoal attach {name}`")
    }

    fn this_place(&self) -> Option<String> {
        std::env::var(SESSION_VAR)
            .ok()
            .filter(|session| !session.is_empty())
    }

    /// The session this process runs in, which the daemon records for its
    /// executions and chats so `shoal attach` finds them there.
    fn origin(&self) -> Option<Origin> {
        Some(Origin {
            host: HostName::Zmx,
            group: None,
            place: Some(self.this_place()?),
            data: BTreeMap::new(),
            owned: false,
        })
    }

    fn place_variables(&self) -> &'static [&'static str] {
        &[SESSION_VAR]
    }

    /// A detached session ends with its agent, leaving no shell.
    fn places_keep_shells(&self) -> bool {
        false
    }
}

impl Watches for Zmx {
    /// zmx reuses session names, so a session is told apart by its process
    /// and creation time.
    fn place_instance<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move {
            let sessions = list().await?;
            Ok(sessions
                .iter()
                .find(|(name, _)| name == place)
                .map(|(_, fields)| instance(fields)))
        })
    }

    /// zmx reads no agent state, so a session is only open or gone. A later
    /// session under the same name counts as gone.
    fn place_state<'a>(&'a self, place: &'a Watched) -> BoxFuture<'a, Result<PlaceState>> {
        Box::pin(async move {
            let sessions = list().await?;
            Ok(if hosts(&sessions, place) {
                PlaceState::Open(None)
            } else {
                PlaceState::Gone
            })
        })
    }
}

impl Closes for Zmx {
    fn close_place<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(kill(place))
    }
}

impl Sessions for Zmx {
    fn run_agent<'a>(
        &'a self,
        start: &'a Start<'a>,
        owner: Owner<'a>,
        agent: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move { run(start, owner, agent).await.map(Some) })
    }

    fn label_current<'a>(&'a self, state: &'a Path, owner: Owner<'a>) -> BoxFuture<'a, Result<()>> {
        Box::pin(label_current(state, owner))
    }

    fn keeps_agents_in_sessions(&self) -> bool {
        on_terminal()
    }

    fn launcher_program(&self) -> Option<&'static str> {
        on_terminal().then(|| Tool::Zmx.program())
    }

    /// The session under the recorded name, while zmx keeps it.
    fn session_at<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<Option<Session>>> {
        Box::pin(session_at(place))
    }

    fn attaches_places(&self) -> bool {
        true
    }

    fn attach<'a>(&'a self, session: &'a Session) -> BoxFuture<'a, Result<()>> {
        Box::pin(attach(&session.name))
    }

    fn left_running<'a>(&'a self, session: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move { Ok(list().await?.iter().any(|(name, _)| name == session)) })
    }
}

impl Prompts for Zmx {
    fn prompt_session(&self) -> Option<OsString> {
        std::env::var_os(SESSION_VAR)
    }

    fn submit_prompt<'a>(&'a self, session: &'a OsStr, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(submit_prompt(session, text))
    }
}

impl Demos for Zmx {
    fn start_demo<'a>(&'a self, demo: &'a Demo<'a>) -> BoxFuture<'a, Result<Origin>> {
        Box::pin(start_demo(demo))
    }

    fn open_demo<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<()>> {
        Box::pin(open_demo(place.id()))
    }

    fn demo_screen<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<String>> {
        Box::pin(demo_screen(place.id()))
    }

    fn send_demo_keys<'a>(&'a self, place: &'a Origin, keys: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(send_keys(place.id(), keys))
    }
}

/// Whether this process could attach a new session: a terminal outside zmx.
fn on_terminal() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::env::var_os(SESSION_VAR).is_none()
}

/// Start `start`, `agent`'s tracked execution for `owner`, in a new session
/// and attach this terminal to it, returning the session. zmx does not
/// report the agent's exit status; the daemon still records the exit.
async fn run(start: &Start<'_>, owner: Owner<'_>, agent: &str) -> Result<String> {
    // Launches check zmx before any work; resumed agents reach it here.
    ensure_program(agent, Tool::Zmx.program())?;
    let name = unused_name(start.label).await?;
    let mut zmx = Command::new(Tool::Zmx.program());
    zmx.arg("attach").arg("--labels").arg(format!(
        "{} {CREATED} {}",
        owner_label(owner),
        crate::sessions::zmx::state_label(start.state)?
    ));
    attach_new(&mut zmx, &name, start);
    ended(zmx.status().await.context("run zmx")?)?;
    Ok(name)
}

/// Run `start`, a chat, in a new session and attach this terminal to it.
/// The session's Shoal records the chat there, labels the session with it and
/// becomes its agent, as it would in place. Returns the session's name.
async fn run_chat(start: &Start<'_>) -> Result<String> {
    let name = unused_name(start.label).await?;
    let mut zmx = Command::new(Tool::Zmx.program());
    zmx.args(["attach", "--labels", CREATED]);
    attach_new(&mut zmx, &name, start);
    ended(zmx.status().await.context("run zmx")?)?;
    Ok(name)
}

/// Have `zmx attach` create the session `name` running `start`'s command.
fn attach_new(zmx: &mut Command, name: &str, start: &Start<'_>) {
    zmx.arg(name)
        .args(start.argv)
        .current_dir(start.cwd)
        .envs(start.env.iter().map(|(name, value)| (name, value)))
        .env_remove(env::SHELL_DIRECTIVE);
    for variable in super::foreign_place_variables(HostName::Zmx) {
        zmx.env_remove(variable);
    }
}

/// A name for a new session called `label` that no session zmx keeps has.
async fn unused_name(label: &str) -> Result<String> {
    let taken = list().await?.into_iter().map(|session| session.0).collect();
    Ok(free_name(label, &taken))
}

/// Run `start` in a new detached session, as if typed at its shell, since
/// zmx gives commands it runs itself no terminal input. That shell is zmx's
/// bash, so the session ends once the command succeeds, which it does once
/// its agent or workspace shell has run, and otherwise becomes the user's
/// login shell to show why it failed. `start`'s variables reach that shell,
/// inert there.
async fn start_detached(start: &Start<'_>) -> Result<()> {
    ensure_program("background launch", Tool::Zmx.program())?;
    let name = unused_name(start.label).await?;
    run_in_new_session(
        &name,
        start.cwd,
        start.argv,
        &NewSession {
            environment: start.env,
            labels: vec![
                CREATED.into(),
                crate::sessions::zmx::state_label(start.state)?,
            ],
            keep_failed_shell: true,
            ..Default::default()
        },
    )
    .await
}

/// Start `demo` in a new detached session, whose worker ends the session
/// once the demo stops.
async fn start_demo(demo: &Demo<'_>) -> Result<Origin> {
    ensure_program("demo", Tool::Zmx.program())?;
    let session = unused_name(demo.label).await?;
    let place = Origin::created(HostName::Zmx, session.clone());
    run_in_new_session(
        &session,
        demo.cwd,
        &(demo.worker)(&place)?,
        &NewSession {
            environment: demo.env,
            clear: super::NOT_INHERITED_BY_DEMOS,
            labels: vec![
                owner_label(Owner::Workspace(demo.workspace)),
                CREATED.into(),
                format!("shoal.demo={}", demo.launch),
                crate::sessions::zmx::state_label(demo.state)?,
            ],
            ..Default::default()
        },
    )
    .await?;
    Ok(place)
}

/// A detached session's shell setup and ownership labels.
#[derive(Default)]
struct NewSession<'a> {
    environment: &'a [(&'a str, String)],
    clear: &'a [&'a str],
    labels: Vec<String>,
    keep_failed_shell: bool,
}

/// Create a session, then hand its shell the worker. A failed handoff closes
/// the session so it cannot leave an idle shell behind. Keep reassignment out
/// until the handoff and any verified error cleanup finish.
async fn run_in_new_session(
    name: &str,
    cwd: &Path,
    argv: &[OsString],
    launch: &NewSession<'_>,
) -> Result<()> {
    let words = worker_words(argv)?;
    let _server = crate::sessions::zmx::ownership_lock().await?;
    create_session(name, cwd, launch).await?;
    let created = cleanup_instance(name).await?;
    let result = start_session_worker(name, &words, launch).await;
    if result.is_err() {
        let _ = close_failed_session(name, created.as_deref()).await;
    }
    result
}

/// A failed handoff may outlive its session. Unknown or replaced instances do
/// not authorize closing whatever now has the name.
async fn close_failed_session(name: &str, created: Option<&str>) -> Result<()> {
    if let Some(created) = created
        && cleanup_instance(name).await?.as_deref() == Some(created)
    {
        kill(name).await?;
    }
    Ok(())
}

async fn cleanup_instance(name: &str) -> Result<Option<String>> {
    Ok(list()
        .await?
        .iter()
        .find(|(session, _)| session == name)
        .and_then(|(_, fields)| {
            field(fields, "pid").filter(|value| !value.is_empty())?;
            field(fields, "created").filter(|value| !value.is_empty())?;
            Some(instance(fields))
        }))
}

fn worker_words(argv: &[OsString]) -> Result<Vec<&str>> {
    argv.iter()
        .map(|arg| arg.to_str().context("Shoal command path is not UTF-8"))
        .collect()
}

async fn create_session(name: &str, cwd: &Path, launch: &NewSession<'_>) -> Result<()> {
    let mut create = Command::new(Tool::Zmx.program());
    create
        .args(["run", name, "-d", "true"])
        .current_dir(cwd)
        .envs(launch.environment.iter().map(|(name, value)| (name, value)))
        .stdin(std::process::Stdio::null());
    for name in launch
        .clear
        .iter()
        .copied()
        .chain(super::foreign_place_variables(HostName::Zmx))
    {
        create.env_remove(name);
    }
    Run::new(create)
        .checked()
        .await
        .context("create a zmx session")?;
    Ok(())
}

async fn start_session_worker(name: &str, words: &[&str], launch: &NewSession<'_>) -> Result<()> {
    label_session(name, &launch.labels).await?;
    send_session_worker(name, words, launch).await
}

async fn label_session(name: &str, labels: &[String]) -> Result<()> {
    if labels.is_empty() {
        return Ok(());
    }
    let mut set = Command::new(Tool::Zmx.program());
    set.args(["set", name]).args(labels);
    Run::new(set)
        .checked()
        .await
        .context("label the zmx session")?;
    Ok(())
}

async fn send_session_worker(name: &str, words: &[&str], launch: &NewSession<'_>) -> Result<()> {
    let mut send = Command::new(Tool::Zmx.program());
    send.args(["send", name])
        .arg(session_command(words, launch));
    Run::new(send)
        .checked()
        .await
        .context("start Shoal in its zmx session")?;
    Ok(())
}

/// A demo always ends its shell; other launches keep failures visible there.
fn session_command(words: &[&str], launch: &NewSession<'_>) -> String {
    // A session zmx started elsewhere may carry these variables anyway.
    let unset = if launch.clear.is_empty() {
        String::new()
    } else {
        format!("unset {}; ", launch.clear.join(" "))
    };
    let finish = if launch.keep_failed_shell {
        " && exit; exec \"$SHELL\" -l"
    } else {
        "; exit"
    };
    format!(
        "{unset}{}=1 {}{finish}\r",
        env::ENDS_SESSION,
        crate::shell::quote(words)
    )
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

/// Label the zmx session this process runs in, when it runs in one, as
/// `owner`'s, so cleanup closes it with `owner` and not with the owner it was
/// created for, which would end `owner`'s agent.
async fn label_current(state: &Path, owner: Owner<'_>) -> Result<()> {
    let Some(session) = std::env::var_os(SESSION_VAR) else {
        return Ok(());
    };
    let _server = crate::sessions::zmx::ownership_lock().await?;
    let mut set = Command::new(Tool::Zmx.program());
    set.arg("set")
        .arg(session)
        .args(replacement_labels(owner).split(' '))
        .arg(crate::sessions::zmx::state_label(state)?);
    Run::new(set)
        .checked()
        .await
        .context("label the zmx session")?;
    Ok(())
}

/// Attach this terminal to `session` until the user detaches or it ends.
async fn attach(session: &str) -> Result<()> {
    ensure_program("attach", Tool::Zmx.program())?;
    let status = Command::new(Tool::Zmx.program())
        .args(["attach", session])
        .status()
        .await
        .context("run zmx")?;
    ended(status)
}

/// Restore the terminal after zmx left it, refusing a failed zmx.
fn ended(status: ExitStatus) -> Result<()> {
    crate::execution::reset_input_modes();
    if !status.success() {
        bail!("zmx exited with {status}");
    }
    Ok(())
}

/// The session `place` names, with its attached terminals, while zmx keeps
/// it.
async fn session_at(place: &Origin) -> Result<Option<Session>> {
    Ok(list()
        .await?
        .into_iter()
        .find(|(name, _)| name == place.id())
        .map(|(name, fields)| Session {
            clients: field(&fields, "clients")
                .and_then(|clients| clients.parse().ok())
                .unwrap_or(0),
            name,
            place: place.clone(),
        }))
}

/// Whether `sessions` list the watched session: under its name, labelled
/// with its workspace, and the instance the watch began with.
fn hosts(sessions: &[(String, Fields)], place: &Watched) -> bool {
    sessions.iter().any(|(name, fields)| {
        *name == place.id
            && field(fields, WORKSPACE_LABEL) == Some(place.workspace.as_str())
            && place
                .instance
                .as_ref()
                .is_none_or(|instance| *instance == self::instance(fields))
    })
}

/// A session's process and creation time.
fn instance(fields: &Fields) -> String {
    let value = |key| field(fields, key).unwrap_or_default();
    format!("{}:{}", value("pid"), value("created"))
}

/// The label naming `owner` as a session's owner.
fn owner_label(owner: Owner<'_>) -> String {
    let key = match owner.kind() {
        OwnerKind::Workspace => WORKSPACE_LABEL,
        OwnerKind::Chat => CHAT_LABEL,
    };
    format!("{key}={}", owner.id())
}

/// Replace Shoal ownership in a reused session without touching user labels.
/// Empty values remove labels in the same zmx set request.
fn replacement_labels(owner: Owner<'_>) -> String {
    let obsolete = match owner.kind() {
        OwnerKind::Workspace => CHAT_LABEL,
        OwnerKind::Chat => WORKSPACE_LABEL,
    };
    format!("{} {obsolete}= shoal.demo=", owner_label(owner))
}

/// The workspace's or chat's name, shortened to fit a socket path, then
/// suffixed `-2`, `-3`, ... past sessions that already use it.
fn free_name(owner: &str, taken: &HashSet<String>) -> String {
    let base: String = owner.chars().take(MAX_NAME_LEN).collect();
    std::iter::once(base.clone())
        .chain((2..).map(|n| format!("{base}-{n}")))
        .find(|name| !taken.contains(name))
        .expect("an unused suffix exists")
}

#[cfg(test)]
mod tests;
