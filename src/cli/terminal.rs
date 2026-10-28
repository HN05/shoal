//! Where Shoal runs work for the caller's terminal. Commands say what they
//! launch; this module chooses whether it takes the terminal or runs in the
//! background, and the [`Host`] that carries that out with its own
//! primitives. Commands never name a host; adding one means implementing
//! [`Host`] and listing it in [`HOSTS`].
mod background;
mod closing;
mod herdr;
mod naming;
mod zmx;

use std::{
    collections::{BTreeMap, HashSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use futures_util::future::BoxFuture;
use serde::Serialize;

use super::{
    commands::{
        issues::Issue,
        workspaces::{AddPlan, execute_add},
    },
    context::Context,
};
use crate::{
    config::Effective,
    demos::Launch,
    env,
    execution::recovery::Record,
    model::{Repository, Workspace},
};

pub(in crate::cli) use closing::{PlaceState, Watched, watch as watch_place};
pub(in crate::cli) use naming::{PlaceName, issue_label};

/// Hosts in the order they claim a terminal, each with the options its own
/// config section gives; a terminal none of them serves runs everything in
/// place.
const HOSTS: &[Registered] = &[
    Registered {
        host: &herdr::Herdr,
        options: |settings| herdr::options(&settings.herdr),
    },
    Registered {
        host: &zmx::Zmx,
        options: |_| Options::DEFAULT,
    },
];

/// A host with the options `settings` give it.
struct Registered {
    host: &'static dyn Host,
    options: fn(&Effective) -> Options,
}

/// Every host, for what it does whatever config says.
fn hosts() -> impl Iterator<Item = &'static dyn Host> {
    HOSTS.iter().map(|registered| registered.host)
}

/// What config sets for a host, from the section of its own it may have.
pub(in crate::cli) struct Options {
    /// Whether the host serves the terminals it runs in.
    pub enabled: bool,
    /// Whether it opens places, such as tabs, beside the caller.
    pub opens_places: bool,
    /// The template for the names of the places it opens, in place of the
    /// default.
    pub name_template: Option<String>,
    /// Whether the places it opens for a workspace close once it is done,
    /// over the top-level `close_when_done`.
    pub close_when_done: Option<bool>,
}

impl Options {
    /// A host without a section: config only sets what all hosts share.
    const DEFAULT: Self = Self {
        enabled: true,
        opens_places: true,
        name_template: None,
        close_when_done: None,
    };
}

/// A host as `settings` set it up.
struct Configured {
    host: &'static dyn Host,
    options: Options,
    /// Whether the places it opens for a workspace close once it is done.
    closes_when_done: bool,
}

impl Configured {
    fn new(host: &'static dyn Host, options: Options, settings: &Effective) -> Self {
        let closes_when_done = options.close_when_done.unwrap_or(settings.close_when_done);
        Self {
            host,
            options,
            closes_when_done,
        }
    }

    /// Every host as `settings` set it up, in the order they claim a terminal.
    fn all(settings: &Effective) -> impl Iterator<Item = Self> + '_ {
        HOSTS
            .iter()
            .map(|registered| Self::new(registered.host, (registered.options)(settings), settings))
    }

    /// The plain terminal, which every terminal falls back to.
    fn plain(settings: &Effective) -> Self {
        let options = Options {
            opens_places: false,
            ..Options::DEFAULT
        };
        Self::new(&Plain, options, settings)
    }

    /// Whether the host serves the terminal this process runs in.
    fn serves(&self) -> bool {
        self.options.enabled && self.host.serves()
    }

    /// Whether a queued start runs here, judged from the environment the
    /// daemon gave its worker, which has no terminal.
    fn takes_queued(&self) -> bool {
        self.options.enabled && self.options.opens_places && self.host.takes_queued()
    }

    /// Whether the host runs Shoal for a workspace in the background.
    fn runs_in_background(&self) -> bool {
        self.options.opens_places && self.host.runs_in_background()
    }
}

/// A terminal host: a multiplexer or terminal app that runs Shoal work beside
/// the caller. A capability a host lacks keeps that work in this process.
pub(in crate::cli) trait Host: Sync {
    /// The name sessions and messages use.
    fn name(&self) -> &'static str;

    /// Whether this host serves the terminal this process runs in, where
    /// config enables it.
    fn serves(&self) -> bool;

    /// Whether a queued start runs here, where config lets it open places,
    /// judged from the environment the daemon gave its worker, which has no
    /// terminal.
    fn takes_queued(&self) -> bool {
        false
    }

    /// Refuse when the programs this host runs are missing.
    fn ensure_installed(&self) -> Result<()> {
        Ok(())
    }

    /// Run `start`, a workspace addition, in a new place, returning whether
    /// it left this process. Generic code calls it only where config lets
    /// the host open places.
    fn start_add<'a>(&'a self, _start: &'a Start<'a>) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// Whether this host runs Shoal for a workspace in the background, where
    /// config lets it open places.
    fn runs_in_background(&self) -> bool {
        false
    }

    /// Run `start` in a new background place.
    fn run_in_background<'a>(&'a self, _start: &'a Start<'a>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot run in the background", self.name()) })
    }

    /// Run `argv` with `env` in `place`, an earlier place of this host, when
    /// it is idle. Returns whether it ran.
    fn run_in_place<'a>(
        &'a self,
        _place: &'a str,
        _argv: &'a [OsString],
        _env: &'a [(&'a str, String)],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// Run `start`, a tracked agent `agent` of `owner`, in a new session and
    /// attach this terminal to it, returning the session once the terminal
    /// left it. `None` means the agent runs in place.
    fn run_agent<'a>(
        &'a self,
        _start: &'a Start<'a>,
        _owner: Owner<'a>,
        _agent: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }

    /// Run `start`, a chat, beside this terminal, where config lets the host
    /// open places.
    fn start_chat<'a>(&'a self, _start: &'a Start<'a>) -> BoxFuture<'a, Result<ChatStart>> {
        Box::pin(async { Ok(ChatStart::Here) })
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

    /// What tells `place` apart from a later place under the same ID, for a
    /// host that reuses IDs.
    fn place_instance<'a>(&'a self, _place: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }

    /// Whether the watched `place` is still open, and its agent's turn as
    /// this host reads it.
    fn place_state<'a>(&'a self, _place: &'a Watched) -> BoxFuture<'a, Result<PlaceState>> {
        Box::pin(async { anyhow::bail!("{} cannot read places", self.name()) })
    }

    /// Whether the places this host opens show a name.
    fn names_places(&self) -> bool {
        false
    }

    /// Show `label` as the name of `place`, one this host opened.
    fn rename_place<'a>(&'a self, _place: &'a str, _label: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// Whether a place this host opens for an addition keeps a shell that
    /// enters the workspace afterwards.
    fn places_keep_shells(&self) -> bool {
        true
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
    /// `shoal attach` finds `owner`'s agent there.
    fn label_current<'a>(
        &'a self,
        _state: &'a Path,
        _owner: Owner<'a>,
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

    /// `owner`'s running agent sessions.
    fn sessions<'a>(&'a self, _owner: Owner<'a>) -> BoxFuture<'a, Result<Vec<Session>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// The IDs of the owners of `kind` with at least one agent session.
    fn session_owners(&self, _kind: OwnerKind) -> BoxFuture<'_, Result<HashSet<String>>> {
        Box::pin(async { Ok(HashSet::new()) })
    }

    /// Attach this terminal to `session`, one of [`Host::sessions`], until
    /// the user detaches or the session ends.
    fn attach<'a>(&'a self, _session: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} keeps no sessions", self.name()) })
    }

    /// Start `demo` in a new background place of this host. Returns the
    /// place's handle, or `None` when this host keeps no demos.
    fn start_demo<'a>(&'a self, _demo: &'a Demo<'a>) -> BoxFuture<'a, Result<Option<String>>> {
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

/// Work a host starts in a new place, which generic code describes in full,
/// so the host only carries it out.
pub(in crate::cli) struct Start<'a> {
    /// The daemon state directory that owns the place.
    pub state: &'a Path,
    /// The directory the place opens at.
    pub cwd: &'a Path,
    /// What the place is called: a tab's label, a session's name.
    pub label: &'a str,
    /// Whether the place takes the caller's terminal.
    pub focus: bool,
    /// The command the place runs.
    pub argv: &'a [OsString],
    /// Variables the command needs from this process, since the place may
    /// start in an environment of its own.
    pub env: &'a [(&'a str, String)],
}

/// A demo a host starts in a new background place, as [`Start`] describes
/// other work; its command depends on the place's handle.
pub(in crate::cli) struct Demo<'a> {
    pub cwd: &'a Path,
    pub label: &'a str,
    pub env: &'a [(&'a str, String)],
    pub worker: &'a DemoWorker<'a>,
}

/// A terminal no host serves: everything runs in place.
struct Plain;

impl Host for Plain {
    fn name(&self) -> &'static str {
        "terminal"
    }

    fn serves(&self) -> bool {
        true
    }

    fn background_places(&self) -> &'static str {
        "the background"
    }

    fn background_note(&self, name: &str) -> String {
        format!("Started {name} in the background")
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
fn current(settings: &Effective) -> Configured {
    Configured::all(settings)
        .find(Configured::serves)
        .unwrap_or_else(|| Configured::plain(settings))
}

/// The host that starts work beside this terminal. A place a host opened for
/// this process, or a terminal without a user, keeps work in place.
fn beside(ctx: &Context, settings: &Effective) -> Configured {
    if ctx.place.is_none() && ctx.interactive() {
        current(settings)
    } else {
        Configured::plain(settings)
    }
}

/// What an agent session runs for, by ID; `shoal attach` finds a
/// workspace's or a chat's sessions by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Owner<'a> {
    Workspace(&'a str),
    Chat(&'a str),
}

/// What kind of [`Owner`] a session has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum OwnerKind {
    Workspace,
    Chat,
}

impl<'a> Owner<'a> {
    pub fn kind(self) -> OwnerKind {
        match self {
            Self::Workspace(_) => OwnerKind::Workspace,
            Self::Chat(_) => OwnerKind::Chat,
        }
    }

    pub fn id(self) -> &'a str {
        match self {
            Self::Workspace(id) | Self::Chat(id) => id,
        }
    }
}

/// Where a host started a chat beside this terminal.
pub(in crate::cli) enum ChatStart {
    /// Nowhere: it runs in place, in this process.
    Here,
    /// In a new tab.
    Tab,
    /// In this new session, which this terminal stayed attached to until the
    /// user detached or the chat ended.
    Session(String),
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
/// `name` is the planned workspace name; `focus` focuses the start whatever
/// the `focus` setting says.
pub(in crate::cli) async fn start_add(
    ctx: &Context,
    settings: &Effective,
    focus: bool,
    plan: &mut AddPlan,
    issue: Option<&Issue>,
    name: &str,
) -> Result<bool> {
    ensure!(
        !env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot allocate workspaces"
    );
    let host = beside(ctx, settings);
    let focus = focus.then_some(true).or(settings.focus);
    let placement = Placement::choose(focus, !plan.runs_unattended());
    naming::name_plan(ctx, host.options.name_template.clone(), plan, issue).await?;
    let started = host.options.opens_places && run_add(ctx, &host, plan, name, placement).await?;
    if started && placement == Placement::Background {
        outln!("{}", host.host.background_note(name));
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
    let host = queued(settings)?;
    naming::name_plan(ctx, host.options.name_template.clone(), plan, None).await?;
    run_add(ctx, &host, plan, name, Placement::Background).await?;
    Ok(())
}

/// Have `host` run the add worker for `plan`, the planned workspace `name`,
/// in a new place as `placement` asks, returning whether it left this
/// process. The worker reads the plan from the place's environment, and a
/// place that keeps a shell gets the directive through which the worker has
/// it enter the workspace.
async fn run_add(
    ctx: &Context,
    host: &Configured,
    plan: &AddPlan,
    name: &str,
    placement: Placement,
) -> Result<bool> {
    let worker = add_worker_argv(ctx, host)?;
    let directive = host
        .host
        .places_keep_shells()
        .then(ShellDirective::create)
        .transpose()?;
    let start = Start {
        state: &ctx.paths.state,
        cwd: &std::env::current_dir()?,
        label: &add_label(host.host, plan, name),
        focus: placement == Placement::Focus,
        argv: &worker,
        env: &add_env(ctx, plan, directive.as_ref())?,
    };
    let started = host.host.start_add(&start).await?;
    if let (true, Some(directive)) = (started, directive) {
        directive.hand_over()?;
    }
    Ok(started)
}

/// What the place `host` opens for `plan` is called: the plan's label where
/// the host shows names, otherwise the planned workspace `name`.
fn add_label(host: &dyn Host, plan: &AddPlan, name: &str) -> String {
    if host.names_places() {
        plan.label()
    } else {
        name.to_owned()
    }
}

/// The variables of the place that adds `plan`: a new place's, the plan,
/// and the `directive` of a place that keeps a shell.
fn add_env(
    ctx: &Context,
    plan: &AddPlan,
    directive: Option<&ShellDirective>,
) -> Result<Vec<(&'static str, String)>> {
    let mut env = place_env(ctx)?;
    env.push((env::ADD_PLAN, serde_json::to_string(plan)?));
    if let Some(directive) = directive {
        env.push((env::SHELL_DIRECTIVE, directive.path()));
    }
    Ok(env)
}

/// The file in which the add worker leaves the workspace for the shell of
/// its place to enter. Shoal removes it until the place runs its worker;
/// then the shell removes it at its next prompt, or the watcher when it
/// closes the place.
struct ShellDirective(tempfile::TempPath);

impl ShellDirective {
    fn create() -> Result<Self> {
        let file = tempfile::Builder::new()
            .prefix("shoal-cd.")
            .tempfile()
            .context("create the place's shell directive")?;
        Ok(Self(file.into_temp_path()))
    }

    fn path(&self) -> String {
        self.0.display().to_string()
    }

    fn hand_over(self) -> Result<()> {
        self.0.keep().context("keep the place's shell directive")?;
        Ok(())
    }
}

/// The variables a command in a new place needs from this process: its
/// output setting, which agents there inherit, and its home and config
/// location, whatever the environment the host starts places in.
fn place_env(ctx: &Context) -> Result<Vec<(&'static str, String)>> {
    let config = crate::config::Config::path(&ctx.paths);
    let config_home = config
        .parent()
        .and_then(Path::parent)
        .context("config has no parent")?;
    Ok(vec![
        (env::OUTPUT, env::output_setting().1),
        ("HOME", ctx.paths.home.display().to_string()),
        ("XDG_CONFIG_HOME", config_home.display().to_string()),
    ])
}

/// This executable with `args`, as a place runs it.
fn shoal_command(ctx: &Context, args: &[OsString]) -> Result<Vec<OsString>> {
    let mut argv = crate::cli::internal::shoal_argv(&ctx.paths)?;
    argv.extend_from_slice(args);
    Ok(argv)
}

/// The host that runs a queued start from this terminal under `settings`.
fn queued(settings: &Effective) -> Result<Configured> {
    Configured::all(settings)
        .find(Configured::takes_queued)
        .context("no terminal host can start queued agents")
}

/// Refuse a background start that the daemon's worker could not run from
/// this terminal under `settings`, such as one whose host is not installed.
pub(in crate::cli) fn ensure_background(settings: &Effective) -> Result<()> {
    queued(settings)?.host.ensure_installed()
}

/// What a start the daemon runs later for this process needs, without a
/// terminal: the variables that reach this terminal's host and the output
/// setting.
pub(crate) fn start_environment() -> BTreeMap<String, String> {
    let mut env: BTreeMap<_, _> = hosts().flat_map(|host| host.environment()).collect();
    env.extend([env::output_setting()]);
    env
}

/// Whether the place this process runs in closes once `workspace` is
/// removed, ending anything it streams.
pub(in crate::cli) fn closes_with(paths: &crate::paths::Paths, workspace: &str) -> bool {
    closing::closes_with(paths, workspace)
}

/// Where an agent running in this process sits, as its host names it, so
/// a later resume can restore it there.
pub(crate) fn current_place() -> Option<String> {
    hosts().find_map(|host| host.current_place())
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
    let host = beside(ctx, settings).host;
    let reused = rerun_in_host(ctx, host, record, args).await?;
    Ok(reused.then(|| host.background_places()))
}

/// Restore the agent of `record` in its earlier place in `host`, when idle.
async fn rerun_in_host(
    ctx: &Context,
    host: &dyn Host,
    record: &Path,
    args: &[OsString],
) -> Result<bool> {
    let Some(place) = stopped_place(host, record) else {
        return Ok(false);
    };
    let argv = shoal_command(ctx, args)?;
    host.run_in_place(&place, &argv, &place_env(ctx)?).await
}

/// Where the agent of `record` stopped, unless that is where this process
/// runs, which is never reused.
fn stopped_place(host: &dyn Host, record: &Path) -> Option<String> {
    std::fs::read(record)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
        .and_then(|record| record.place)
        .filter(|place| host.current_place().as_ref() != Some(place))
}

/// Run Shoal with `args`, which restore the agent of `record` in
/// `workspace`, for the daemon's worker, which has no terminal: where the
/// agent stopped when that place is back at its prompt, otherwise in the
/// background of the first host that runs queued starts and can.
pub(in crate::cli) async fn restore_queued(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    record: &Path,
    args: &[OsString],
) -> Result<()> {
    if rerun_in_host(ctx, current(settings).host, record, args).await? {
        return Ok(());
    }
    let mut failure = None;
    for host in Configured::all(settings).filter(Configured::takes_queued) {
        match start_in_background(ctx, &host, workspace, args).await {
            Ok(()) => return Ok(()),
            // Herdr may be gone, as after a reboot.
            Err(error) => failure = Some(error),
        }
    }
    Err(failure.unwrap_or_else(|| anyhow::anyhow!("no terminal host can start queued agents")))
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
    if !host.runs_in_background() {
        return Ok(None);
    }
    start_in_background(ctx, &host, workspace, args).await?;
    Ok(Some(host.host.background_places()))
}

/// Run Shoal with `args` for `workspace` in a new background place of `host`.
async fn start_in_background(
    ctx: &Context,
    host: &Configured,
    workspace: &Workspace,
    args: &[OsString],
) -> Result<()> {
    let template = host.options.name_template.clone();
    let label = naming::workspace_label(ctx, template, host.host, workspace).await;
    let start = Start {
        state: &ctx.paths.state,
        cwd: &workspace.path,
        label: &label,
        focus: false,
        argv: &shoal_command(ctx, args)?,
        env: &place_env(ctx)?,
    };
    host.host.run_in_background(&start).await
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
    let Configured {
        host,
        closes_when_done,
        ..
    } = current(settings);
    let worker = crate::cli::internal::internal_command(
        &ctx.paths,
        false,
        crate::cli::internal::Worker::Session {
            workspace: &workspace.id,
            agent,
            records,
            command,
            close_in: closes_when_done.then(|| host.name()),
        },
    )?;
    let mut env = place_env(ctx)?;
    if let Some(entry) = env::queue_entry() {
        env.push((env::QUEUE_ENTRY, entry.to_owned()));
    }
    let start = Start {
        state: &ctx.paths.state,
        cwd: &workspace.path,
        label: &workspace.name,
        focus: true,
        argv: &worker,
        env: &env,
    };
    let owner = Owner::Workspace(&workspace.id);
    let Some(session) = host.run_agent(&start, owner, agent).await? else {
        return Ok(None);
    };
    report_detach(owner, &workspace.name, &session).await?;
    Ok(Some(0))
}

/// Close the place of the host named `host` that this process runs in once
/// `workspace` is done. Without a watcher the place stays open, so a
/// failure only warns.
pub(in crate::cli) async fn close_here_when_done(ctx: &Context, host: &str, workspace: &str) {
    if let Err(error) = watch_here(ctx, host, workspace).await {
        eprintln!("warning: {error:#}");
    }
}

async fn watch_here(ctx: &Context, host: &str, workspace: &str) -> Result<()> {
    let host = named(host).with_context(|| format!("unknown terminal host {host}"))?;
    let place = host
        .this_place()
        .with_context(|| format!("{} did not name the place this agent runs in", host.name()))?;
    closing::start_watcher(ctx, host, &place, workspace).await
}

/// Run Shoal with `args` for the chat `name` at the repository's checkout:
/// in a new focused tab, or in a new session this terminal attaches to, as
/// `run_agent` does for tracked agents. Returns whether it ran there; `false`
/// means the chat runs in place.
pub(in crate::cli) async fn start_chat(
    ctx: &Context,
    settings: &Effective,
    repository: &Repository,
    name: &str,
    args: &[OsString],
) -> Result<bool> {
    let host = beside(ctx, settings);
    if !host.options.opens_places {
        return Ok(false);
    }
    let start = Start {
        state: &ctx.paths.state,
        cwd: &repository.path,
        label: name,
        focus: true,
        argv: &shoal_command(ctx, args)?,
        env: &place_env(ctx)?,
    };
    let started = host.host.start_chat(&start).await?;
    match started {
        ChatStart::Here => Ok(false),
        ChatStart::Tab => Ok(true),
        ChatStart::Session(session) => {
            let chats = super::client::chats(&ctx.paths).await?;
            if let Some(chat) = chats.iter().find(|chat| chat.name == name) {
                report_detach(Owner::Chat(&chat.id), name, &session).await?;
            }
            Ok(true)
        }
    }
}

/// Every host's running agent sessions for `owner`.
pub(in crate::cli) async fn sessions(owner: Owner<'_>) -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    for host in hosts() {
        sessions.extend(host.sessions(owner).await?);
    }
    Ok(sessions)
}

/// The IDs of the owners of `kind` with at least one agent session in any host.
pub(in crate::cli) async fn session_owners(kind: OwnerKind) -> Result<HashSet<String>> {
    let mut owners = HashSet::new();
    for host in hosts() {
        owners.extend(host.session_owners(kind).await?);
    }
    Ok(owners)
}

/// Attach this terminal to `session` of `owner` through the host that keeps
/// it, then say how to return to it by `name` when it outlives the attach.
pub(in crate::cli) async fn attach(owner: Owner<'_>, name: &str, session: &Session) -> Result<i32> {
    let host = named(session.host).context("the session's host is unknown")?;
    host.attach(&session.name).await?;
    report_detach(owner, name, &session.name).await?;
    Ok(0)
}

/// Say how to return to `owner`, named `name`, when its `session` still runs
/// after this terminal left it.
async fn report_detach(owner: Owner<'_>, name: &str, session: &str) -> Result<()> {
    if sessions(owner)
        .await?
        .iter()
        .any(|running| running.name == session)
    {
        eprintln!("shoal: detached from {session}; run `shoal attach {name}` to return");
    }
    Ok(())
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
    let hosts = Configured::all(settings)
        .filter(|host| host.takes_queued() && host.host.ensure_installed().is_ok())
        .map(|host| host.host);
    let env = place_env(ctx)?;
    for host in hosts {
        let demo = Demo {
            cwd: &workspace.path,
            label: &demo_label(host, workspace, name),
            env: &env,
            worker: &|place: &str| worker(host.name(), place),
        };
        if let Some(place) = host.start_demo(&demo).await? {
            return Ok((host.name(), place));
        }
    }
    let worker = |place: &str| worker(Plain.name(), place);
    let place = background::start_demo(&ctx.paths, workspace, name, &worker)?;
    Ok((Plain.name(), place))
}

/// What the place `host` opens for demo `name` of `workspace` is called:
/// readable where the host shows names, otherwise an identifier.
fn demo_label(host: &dyn Host, workspace: &Workspace, name: &str) -> String {
    if host.names_places() {
        format!("{} demo {name}", workspace.name)
    } else {
        format!("{}-{name}", workspace.name)
    }
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
    hosts()
        .chain([&Plain as &dyn Host])
        .find(|host| host.name() == name)
}

/// The place a host opened for this process to add a workspace in, hosting
/// it in place of the caller's shell.
pub(in crate::cli) struct Place {
    host: &'static dyn Host,
    /// The place as [`Host::this_place`] names it.
    id: String,
    closes_when_done: bool,
}

/// The worker that adds the workspace `plan` describes in the place `host`
/// opened for it, closing the place with the workspace when config says so.
/// The plan reaches it in the place's environment.
fn add_worker_argv(ctx: &Context, host: &Configured) -> Result<Vec<OsString>> {
    crate::cli::internal::internal_command(
        &ctx.paths,
        false,
        crate::cli::internal::Worker::Add {
            host: host.host.name(),
            close_when_done: host.closes_when_done,
        },
    )
}

/// The add worker: add the workspace `payload` plans in the place of the
/// host named `host` this process runs in.
pub(in crate::cli) async fn add_worker(
    mut ctx: Context,
    host: &str,
    closes_when_done: bool,
    payload: &str,
) -> Result<i32> {
    let plan = serde_json::from_str(payload).context("invalid workspace plan")?;
    let host = named(host).with_context(|| format!("unknown terminal host {host}"))?;
    let id = host
        .this_place()
        .with_context(|| format!("{} did not name the place this worker runs in", host.name()))?;
    ctx.place = Some(Place {
        host,
        id,
        closes_when_done,
    });
    execute_add(&ctx, plan).await
}

/// Tell the place this process runs in that it hosts `workspace`, whose
/// `agent` starts there: it takes `label`, and closes with the workspace
/// when it should. A session the user opened is labelled once an agent
/// starts in it.
pub(in crate::cli) async fn host_workspace(
    ctx: &Context,
    workspace: &Workspace,
    agent: Option<&str>,
    label: &str,
) -> Result<()> {
    let labelled = match (&ctx.place, agent) {
        // A place is labelled before it is watched, since the watcher only
        // closes a place it finds hosting the workspace.
        (Some(place), _) => {
            let labelled = place
                .host
                .label_current(
                    &ctx.paths.state,
                    Owner::Workspace(&workspace.id),
                    agent.unwrap_or_default(),
                )
                .await;
            place.host.rename_place(&place.id, label).await;
            if place.closes_when_done {
                closing::start_watcher(ctx, place.host, &place.id, &workspace.id).await?;
            }
            labelled
        }
        (None, Some(agent)) => {
            label_current(&ctx.paths, Owner::Workspace(&workspace.id), agent).await
        }
        (None, None) => Ok(()),
    };
    if let Err(error) = labelled {
        eprintln!("warning: {error:#}");
    }
    Ok(())
}

/// Whether the shell that started this process enters a new workspace
/// afterwards: the caller's own or that of the place opened for it.
pub(in crate::cli) fn follows_caller(ctx: &Context) -> bool {
    ctx.place
        .as_ref()
        .is_none_or(|place| place.host.places_keep_shells())
}

/// Whether an agent launched here runs in a session this terminal attaches
/// to, leaving the caller's shell where it was once the terminal detaches.
pub(in crate::cli) fn keeps_agents_in_sessions(settings: &Effective) -> bool {
    current(settings).host.keeps_agents_in_sessions()
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
/// `shoal attach` finds `owner`'s agent there.
pub(in crate::cli) async fn label_current(
    paths: &crate::paths::Paths,
    owner: Owner<'_>,
    agent: &str,
) -> Result<()> {
    for host in hosts() {
        host.label_current(&paths.state, owner, agent).await?;
    }
    Ok(())
}

/// The program an agent launch here needs besides the agent's own.
pub(in crate::cli) fn launcher_program() -> Option<&'static str> {
    hosts().find_map(|host| host.launcher_program())
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
        hosts().find_map(|host| host.prompt_session().map(|name| Self { host, name }))
    }

    /// Submit `text` as a prompt in the session.
    pub(crate) async fn submit(&self, text: &str) -> Result<()> {
        self.host.submit_prompt(&self.name, text).await
    }
}

/// Whether an agent launched here runs in a session its wrapper can type a
/// prompt into: a new one, or the session this process already runs in.
pub(in crate::cli) fn agents_can_be_prompted(settings: &Effective) -> bool {
    keeps_agents_in_sessions(settings) || PromptSession::current().is_some()
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

#[cfg(test)]
mod tests;
