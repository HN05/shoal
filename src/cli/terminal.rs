//! Where Shoal runs work for the caller's terminal. Commands say what they
//! launch; this module chooses whether it takes the terminal or runs in the
//! background, and the [`Host`] that carries that out with its own
//! primitives. Commands never name a host; adding one means implementing
//! [`Host`] and listing it in [`HOSTS`].
mod background;
mod closing;
mod herdr;
mod zmx;

use std::{
    collections::{BTreeMap, HashSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use super::{
    commands::{issues::Issue, workspaces::AddPlan},
    context::Context,
};
use crate::{
    config::Effective,
    demos::Launch,
    env,
    model::{Repository, Workspace},
};

pub(in crate::cli) use closing::{PlaceState, watch as watch_place};
pub(in crate::cli) use herdr::worker as herdr_worker;
pub(in crate::cli) use zmx::worker as session_worker;

/// Hosts in the order they claim a terminal; a terminal none of them serves
/// runs everything in place.
const HOSTS: &[&dyn Host] = &[&herdr::Herdr, &zmx::Zmx];

/// A terminal host: a multiplexer or terminal app that runs Shoal work beside
/// the caller. A capability a host lacks keeps that work in this process.
pub(in crate::cli) trait Host: Sync {
    /// The name sessions and messages use.
    fn name(&self) -> &'static str;

    /// Whether this host serves the terminal this process runs in.
    fn serves(&self, ctx: &Context, settings: &Effective) -> bool;

    /// Whether a queued start runs here, judged from the environment the
    /// daemon gave its worker, which has no terminal.
    fn takes_queued(&self, _settings: &Effective) -> bool {
        false
    }

    /// Refuse when the programs this host runs are missing.
    fn ensure_installed(&self) -> Result<()> {
        Ok(())
    }

    /// Start `plan` as `placement` asks, returning whether it left this
    /// process. `name` is the planned workspace name.
    fn start_add<'a>(
        &'a self,
        _ctx: &'a Context,
        _settings: &'a Effective,
        _plan: &'a mut AddPlan,
        _issue: Option<&'a Issue>,
        _name: &'a str,
        _placement: Placement,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// Run Shoal with `args` for `workspace` in the background, returning
    /// whether it started.
    fn run_in_background<'a>(
        &'a self,
        _ctx: &'a Context,
        _settings: &'a Effective,
        _workspace: &'a Workspace,
        _args: &'a [OsString],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// Run Shoal with `args` where the agent of `record` stopped, when that
    /// place is another one still open and idle. Returns whether it ran.
    fn rerun_where_stopped<'a>(
        &'a self,
        _ctx: &'a Context,
        _record: &'a Path,
        _args: &'a [OsString],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// Run a tracked agent's `command` in a new session and attach this
    /// terminal to it. `None` means the agent runs in place.
    fn run_agent<'a>(
        &'a self,
        _ctx: &'a Context,
        _workspace: &'a Workspace,
        _agent: &'a str,
        _records: &'a [PathBuf],
        _command: &'a [OsString],
    ) -> BoxFuture<'a, Result<Option<i32>>> {
        Box::pin(async { Ok(None) })
    }

    /// Run Shoal with `args` for a chat at the repository's checkout beside
    /// this terminal, returning whether it ran there.
    fn start_chat<'a>(
        &'a self,
        _ctx: &'a Context,
        _settings: &'a Effective,
        _repository: &'a Repository,
        _agent: &'a str,
        _label: &'a str,
        _args: &'a [OsString],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// What several background starts run in.
    fn background_places(&self) -> &'static str;

    /// How a background start of `name` is reported: where it runs and how
    /// to reach it.
    fn background_note(&self, name: &str) -> String;

    // The capabilities below belong to the host this process runs in,
    // whichever host serves its terminal.

    /// Variables a later start needs to reach this host.
    fn environment(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// Where an agent running in this process sits, as this host names it.
    fn current_place(&self) -> Option<String> {
        None
    }

    /// The place this process runs in, as [`Host::close_place`] names it.
    fn this_place(&self) -> Option<String> {
        None
    }

    /// Close `place`, one this host opened for a workspace.
    fn close_place<'a>(&'a self, _place: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot close places", self.name()) })
    }

    /// Whether `place` is still open, and its agent's turn as this host
    /// reads it.
    fn place_state<'a>(&'a self, _place: &'a str) -> BoxFuture<'a, Result<PlaceState>> {
        Box::pin(async { anyhow::bail!("{} cannot read places", self.name()) })
    }

    /// The session this process runs in, where a prompt can be typed.
    fn prompt_session(&self) -> Option<OsString> {
        None
    }

    /// Submit `text` as a prompt in `session`, from [`Host::prompt_session`].
    fn submit_prompt<'a>(
        &'a self,
        _session: &'a OsStr,
        _text: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot type prompts", self.name()) })
    }

    /// Label the session this process runs in, when it runs in one, so
    /// `shoal attach` finds the workspace's agent there.
    fn label_current<'a>(
        &'a self,
        _workspace_id: &'a str,
        _agent: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Whether agents launched here run in sessions their wrapper can type
    /// prompts into.
    fn keeps_agents_in_sessions(&self) -> bool {
        false
    }

    /// The program an agent launch here needs besides the agent's own.
    fn launcher_program(&self) -> Option<&'static str> {
        None
    }

    /// The workspace's running agent sessions.
    fn sessions<'a>(&'a self, _workspace_id: &'a str) -> BoxFuture<'a, Result<Vec<Session>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// The IDs of workspaces with at least one agent session.
    fn session_workspaces(&self) -> BoxFuture<'_, Result<HashSet<String>>> {
        Box::pin(async { Ok(HashSet::new()) })
    }

    /// Attach this terminal to `session`, one of [`Host::sessions`].
    fn attach<'a>(
        &'a self,
        _workspace: &'a Workspace,
        _session: &'a str,
    ) -> BoxFuture<'a, Result<i32>> {
        Box::pin(async { anyhow::bail!("{} keeps no sessions", self.name()) })
    }

    /// Start a demo of `workspace` in a new place of this host named after
    /// `name`, running the demo worker `worker` gives for the place's handle.
    /// Returns the handle, or `None` when this host keeps no demos.
    fn start_demo<'a>(
        &'a self,
        _ctx: &'a Context,
        _workspace: &'a Workspace,
        _name: &'a str,
        _worker: &'a DemoWorker<'a>,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }

    /// Show the demo at `place` in this terminal until the user leaves it.
    fn open_demo<'a>(&'a self, _place: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot show demos", self.name()) })
    }

    /// What the demo at `place` shows, oldest line first.
    fn demo_screen<'a>(&'a self, _place: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { anyhow::bail!("{} cannot read a demo's screen", self.name()) })
    }

    /// Type `keys` into the demo at `place`.
    fn send_demo_keys<'a>(&'a self, _place: &'a str, _keys: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot type into a demo", self.name()) })
    }
}

/// Variables a demo's worker must not inherit from the agent that starts it:
/// the agent's execution marker would make the agent's exit stop the demo.
const NOT_INHERITED_BY_DEMOS: &[&str] = &[env::EXECUTION_ID];

/// The demo worker's argv for the handle of the place it runs in.
pub(in crate::cli) type DemoWorker<'a> = dyn Fn(&str) -> Result<Vec<OsString>> + Send + Sync + 'a;

/// A terminal no host serves: everything runs in place.
struct Plain;

impl Host for Plain {
    fn name(&self) -> &'static str {
        "terminal"
    }

    fn serves(&self, _ctx: &Context, _settings: &Effective) -> bool {
        true
    }

    fn background_places(&self) -> &'static str {
        "the background"
    }

    fn background_note(&self, name: &str) -> String {
        format!("Started {name} in the background")
    }

    fn start_demo<'a>(
        &'a self,
        ctx: &'a Context,
        workspace: &'a Workspace,
        name: &'a str,
        worker: &'a DemoWorker<'a>,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(
            async move { background::start_demo(&ctx.paths, workspace, name, worker).map(Some) },
        )
    }

    fn open_demo<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(background::open_demo(place))
    }

    fn demo_screen<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { background::demo_screen(place) })
    }

    fn send_demo_keys<'a>(&'a self, _place: &'a str, _keys: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async {
            anyhow::bail!(
                "a demo running in the background takes no keys; start it from zmx or Herdr"
            )
        })
    }
}

/// The host serving this terminal under `settings`.
fn current(ctx: &Context, settings: &Effective) -> &'static dyn Host {
    HOSTS
        .iter()
        .copied()
        .find(|host| host.serves(ctx, settings))
        .unwrap_or(&Plain)
}

/// The host that starts work beside this terminal. A place a host opened for
/// this process, or a terminal without a user, keeps work in place.
fn beside(ctx: &Context, settings: &Effective) -> &'static dyn Host {
    if ctx.place.is_none() && ctx.interactive() {
        current(ctx, settings)
    } else {
        &Plain
    }
}

/// A running agent session that `shoal attach` can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub name: String,
    pub agent: Option<String>,
    /// Terminals attached to the session.
    pub clients: u32,
    /// The host keeping the session.
    #[serde(skip)]
    host: &'static str,
}

impl Session {
    /// The name, agent, and whether a terminal is attached.
    pub fn label(&self) -> String {
        let mut label = self.name.clone();
        if let Some(agent) = &self.agent {
            label.push_str(&format!("  {agent}"));
        }
        if self.clients > 0 {
            label.push_str("  attached");
        }
        label
    }
}

/// Start `plan` beside this terminal: focused when it waits for the user,
/// otherwise in the background, unless the `focus` setting says otherwise.
/// Returns whether it left this process; a focused launch without a new tab
/// stays here, where its agent runs in a session this terminal attaches to.
/// `name` is the planned workspace name.
pub(in crate::cli) async fn start_add(
    ctx: &Context,
    settings: &Effective,
    plan: &mut AddPlan,
    issue: Option<&Issue>,
    name: &str,
) -> Result<bool> {
    ensure!(
        !env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot allocate workspaces"
    );
    let host = beside(ctx, settings);
    let placement = Placement::choose(settings.focus, !plan.runs_unattended());
    let started = host
        .start_add(ctx, settings, plan, issue, name, placement)
        .await?;
    if started && placement == Placement::Background {
        outln!("{}", host.background_note(name));
    }
    Ok(started)
}

/// Start an admitted queue entry's `plan` in the background where the
/// terminal that queued it would have; the daemon's worker has no terminal.
pub(in crate::cli) async fn start_queued(
    ctx: &Context,
    settings: &Effective,
    plan: &mut AddPlan,
    name: &str,
) -> Result<()> {
    queued(settings)?
        .start_add(ctx, settings, plan, None, name, Placement::Background)
        .await?;
    Ok(())
}

/// The host that runs a queued start from this terminal under `settings`.
fn queued(settings: &Effective) -> Result<&'static dyn Host> {
    HOSTS
        .iter()
        .copied()
        .find(|host| host.takes_queued(settings))
        .context("no terminal host can start queued agents")
}

/// Refuse a background start that the daemon's worker could not run from
/// this terminal under `settings`, such as one whose host is not installed.
pub(in crate::cli) fn ensure_background(settings: &Effective) -> Result<()> {
    queued(settings)?.ensure_installed()
}

/// The variables a later start needs to reach this terminal's host, such
/// as a queued start the daemon runs without a terminal.
pub(in crate::cli) fn environment() -> BTreeMap<String, String> {
    HOSTS.iter().flat_map(|host| host.environment()).collect()
}

/// Whether the place this process runs in closes once `workspace` is
/// removed, ending anything it streams.
pub(in crate::cli) fn closes_with(paths: &crate::paths::Paths, workspace: &str) -> bool {
    closing::closes_with(paths, workspace)
}

/// Where an agent running in this process sits, as its host names it, so
/// a later resume can restore it there.
pub(crate) fn current_place() -> Option<String> {
    HOSTS.iter().find_map(|host| host.current_place())
}

/// Run Shoal with `args` where the agent of `record` stopped, when that
/// place is another one still open and idle. Returns what such restarts run
/// in, or `None` when it did not run.
pub(in crate::cli) async fn rerun_where_stopped(
    ctx: &Context,
    settings: &Effective,
    record: &Path,
    args: &[OsString],
) -> Result<Option<&'static str>> {
    let host = beside(ctx, settings);
    let reused = host.rerun_where_stopped(ctx, record, args).await?;
    Ok(reused.then(|| host.background_places()))
}

/// Run Shoal with `args` for `workspace` in the background, returning what
/// such starts run in, or `None` when this terminal cannot start them.
pub(in crate::cli) async fn run_in_background(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    args: &[OsString],
) -> Result<Option<&'static str>> {
    let host = beside(ctx, settings);
    let started = host
        .run_in_background(ctx, settings, workspace, args)
        .await?;
    Ok(started.then(|| host.background_places()))
}

/// Run a tracked agent's `command` in a new session and attach this terminal
/// to it, when this terminal keeps agents in sessions. `None` means the agent
/// runs in place in this process. `records` are recovery records the launch
/// consumes once it starts.
pub(in crate::cli) async fn run_agent(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    agent: &str,
    records: &[PathBuf],
    command: &[OsString],
) -> Result<Option<i32>> {
    current(ctx, settings)
        .run_agent(ctx, workspace, agent, records, command)
        .await
}

/// Run Shoal with `args` for a chat of `agent` at the repository's checkout:
/// in a new focused tab, or in a new session this terminal attaches to, as
/// `run_agent` does for tracked agents. Returns whether it ran there; `false`
/// means the chat runs in place.
pub(in crate::cli) async fn start_chat(
    ctx: &Context,
    settings: &Effective,
    repository: &Repository,
    agent: &str,
    label: &str,
    args: &[OsString],
) -> Result<bool> {
    beside(ctx, settings)
        .start_chat(ctx, settings, repository, agent, label, args)
        .await
}

/// Every host's running agent sessions for the workspace.
pub(in crate::cli) async fn sessions(workspace_id: &str) -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    for host in HOSTS {
        sessions.extend(host.sessions(workspace_id).await?);
    }
    Ok(sessions)
}

/// The IDs of workspaces with at least one agent session in any host.
pub(in crate::cli) async fn session_workspaces() -> Result<HashSet<String>> {
    let mut workspaces = HashSet::new();
    for host in HOSTS {
        workspaces.extend(host.session_workspaces().await?);
    }
    Ok(workspaces)
}

/// Attach this terminal to `session` through the host that keeps it.
pub(in crate::cli) async fn attach(workspace: &Workspace, session: &Session) -> Result<i32> {
    let host = named(session.host).context("the session's host is unknown")?;
    host.attach(workspace, &session.name).await
}

/// Start a demo of `workspace` in the first host that runs queued starts and
/// keeps demos, otherwise as a background process with a log. `worker` gives
/// the demo worker's argv for the host's name and the place's handle.
/// Returns the host's name and the demo's place there.
pub(in crate::cli) async fn start_demo(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    name: &str,
    worker: &(dyn Fn(&str, &str) -> Result<Vec<OsString>> + Send + Sync),
) -> Result<(&'static str, String)> {
    let hosts = HOSTS
        .iter()
        .copied()
        .filter(|host| host.takes_queued(settings) && host.ensure_installed().is_ok())
        .chain([&Plain as &dyn Host]);
    for host in hosts {
        let worker = |place: &str| worker(host.name(), place);
        if let Some(place) = host.start_demo(ctx, workspace, name, &worker).await? {
            return Ok((host.name(), place));
        }
    }
    unreachable!("the plain host keeps demos")
}

/// Show the demo `launch` describes in this terminal.
pub(in crate::cli) async fn open_demo(launch: &Launch) -> Result<()> {
    demo_host(&launch.host)?.open_demo(&launch.place).await
}

/// What the demo `launch` describes shows.
pub(in crate::cli) async fn demo_screen(launch: &Launch) -> Result<String> {
    demo_host(&launch.host)?.demo_screen(&launch.place).await
}

/// Type `keys` into the demo `launch` describes.
pub(in crate::cli) async fn send_demo_keys(launch: &Launch, keys: &str) -> Result<()> {
    demo_host(&launch.host)?
        .send_demo_keys(&launch.place, keys)
        .await
}

fn demo_host(name: &str) -> Result<&'static dyn Host> {
    named(name).with_context(|| format!("the demo's host {name} is unknown"))
}

/// The host, or the plain terminal, that goes by `name`.
fn named(name: &str) -> Option<&'static dyn Host> {
    HOSTS
        .iter()
        .copied()
        .chain([&Plain as &dyn Host])
        .find(|host| host.name() == name)
}

/// The place a host opened for this process to add a workspace in, hosting
/// it in place of the caller's shell.
pub(in crate::cli) trait Place: Send + Sync {
    /// Tell the place that it hosts `workspace`, whose `agent` starts there,
    /// labelled `label`.
    fn host_workspace<'a>(
        &'a self,
        ctx: &'a Context,
        workspace: &'a Workspace,
        agent: Option<&'a str>,
        label: &'a str,
    ) -> BoxFuture<'a, Result<()>>;

    /// Whether a shell remains here to enter the workspace afterwards.
    fn keeps_shell(&self) -> bool {
        true
    }
}

/// Tell the place this process runs in that it hosts `workspace`, whose
/// `agent` starts there. A session the user opened is labelled once an agent
/// starts in it.
pub(in crate::cli) async fn host_workspace(
    ctx: &Context,
    workspace: &Workspace,
    agent: Option<&str>,
    label: &str,
) -> Result<()> {
    match (&ctx.place, agent) {
        (Some(place), _) => place.host_workspace(ctx, workspace, agent, label).await,
        (None, Some(agent)) => {
            if let Err(error) = label_current(&workspace.id, agent).await {
                eprintln!("warning: {error:#}");
            }
            Ok(())
        }
        (None, None) => Ok(()),
    }
}

/// Whether the shell that started this process enters a new workspace
/// afterwards: the caller's own or that of the place opened for it.
pub(in crate::cli) fn follows_caller(ctx: &Context) -> bool {
    ctx.place.as_ref().is_none_or(|place| place.keeps_shell())
}

/// Whether an agent launched here runs in a session this terminal attaches
/// to, leaving the caller's shell where it was once the terminal detaches.
pub(in crate::cli) fn keeps_agents_in_sessions(ctx: &Context, settings: &Effective) -> bool {
    current(ctx, settings).keeps_agents_in_sessions()
}

/// Give a workspace added without an agent a shell at `cwd` in the place
/// opened for it, which stays open while the shell runs. The caller's own
/// terminal gets its shell back instead.
pub(in crate::cli) async fn open_shell(ctx: &Context, cwd: &Path) -> Result<i32> {
    if ctx.place.is_none() {
        return Ok(0);
    }
    let shell = std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let status = tokio::process::Command::new(shell)
        .arg("-i")
        .current_dir(cwd)
        .env_remove(env::SHELL_DIRECTIVE)
        .status()
        .await
        .context("open workspace shell")?;
    Ok(env::session_exit_code(crate::execution::exit_code(status)))
}

/// Label the session this process runs in, when it runs in one, so
/// `shoal attach` finds the workspace's agent there.
pub(in crate::cli) async fn label_current(workspace_id: &str, agent: &str) -> Result<()> {
    for host in HOSTS {
        host.label_current(workspace_id, agent).await?;
    }
    Ok(())
}

/// The program an agent launch here needs besides the agent's own.
pub(in crate::cli) fn launcher_program() -> Option<&'static str> {
    HOSTS.iter().find_map(|host| host.launcher_program())
}

/// The session this process runs in, where an agent's wrapper can type a
/// prompt for it.
#[derive(Clone)]
pub(crate) struct PromptSession {
    host: &'static dyn Host,
    name: OsString,
}

impl PromptSession {
    pub(crate) fn current() -> Option<Self> {
        HOSTS
            .iter()
            .find_map(|host| host.prompt_session().map(|name| Self { host: *host, name }))
    }

    /// Submit `text` as a prompt in the session.
    pub(crate) async fn submit(&self, text: &str) -> Result<()> {
        self.host.submit_prompt(&self.name, text).await
    }
}

/// Whether an agent launched here runs in a session its wrapper can type a
/// prompt into: a new one, or the session this process already runs in.
pub(in crate::cli) fn agents_can_be_prompted(ctx: &Context, settings: &Effective) -> bool {
    keeps_agents_in_sessions(ctx, settings) || PromptSession::current().is_some()
}

/// Whether a launch takes the caller's terminal or runs in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Placement {
    Focus,
    Background,
}

impl Placement {
    /// The `focus` setting, otherwise focus a launch that waits for the user.
    fn choose(setting: Option<bool>, waits_for_user: bool) -> Self {
        if setting.unwrap_or(waits_for_user) {
            Self::Focus
        } else {
            Self::Background
        }
    }
}

/// The name a host gives the place hosting a new workspace: a template with
/// the repository and issue filled in, rendered once the branch is known.
#[derive(Serialize, Deserialize)]
pub(in crate::cli) struct PlaceName {
    template: String,
    repo: String,
    issue_number: Option<u64>,
    issue_title: Option<String>,
}

impl PlaceName {
    fn new(template: String, repo: &Repository, issue: Option<&Issue>) -> Self {
        Self {
            template,
            repo: crate::repository::name(repo).to_owned(),
            issue_number: issue.map(|issue| issue.number),
            issue_title: issue.map(|issue| issue.title.clone()),
        }
    }

    pub fn render(&self, branch: &str) -> String {
        crate::config::templates::render(
            &self.template,
            &[
                ("{repo}", &self.repo),
                ("{branch}", branch),
                (
                    "{issue_number}",
                    &self.issue_number.map(|n| n.to_string()).unwrap_or_default(),
                ),
                (
                    "{issue_title}",
                    self.issue_title.as_deref().unwrap_or_default(),
                ),
            ],
        )
    }
}

#[cfg(test)]
mod tests;
