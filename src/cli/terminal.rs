//! Where Shoal runs work for the caller's terminal. Commands say what they
//! launch; this module chooses whether it takes the terminal or runs in the
//! background, and the [`Host`] that carries that out with its own
//! primitives. Commands never name a host; adding one means implementing
//! [`Host`] and listing it in [`HOSTS`].
mod background;
mod closing;
mod groups;
mod herdr;
mod naming;
mod rex;
mod typed;
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
    config::{Effective, repo::DefaultHost},
    demos::Launch,
    env,
    model::{Repository, Workspace},
    sessions::{HostName, Origin},
};

pub(in crate::cli) use closing::{PlaceState, Watched, watch as watch_place};
pub(in crate::cli) use naming::{PlaceName, issue_label};
pub(in crate::cli) use typed::run as run_launch;

/// Hosts in the order they claim a terminal, each with the options its own
/// config section gives; a terminal none of them serves runs everything in
/// place.
const HOSTS: &[Registered] = &[
    Registered {
        host: &herdr::Herdr,
        options: |settings| herdr::options(&settings.herdr),
    },
    Registered {
        host: &rex::Rex,
        options: |settings| rex::options(&settings.rex),
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
    /// The template for the name of the group it opens places in when it
    /// has none from the caller, for a host that keeps groups.
    pub group_template: Option<String>,
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
        group_template: None,
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

    /// The host named `name`, or the plain terminal.
    fn named(settings: &Effective, name: HostName) -> Self {
        Self::all(settings)
            .find(|host| host.host.name() == name)
            .unwrap_or_else(|| Self::plain(settings))
    }

    /// The host `default_host` names for a terminal no host serves, or zmx
    /// where config turns that host off.
    fn default_host(settings: &Effective) -> Self {
        let named = Self::named(
            settings,
            match settings.default_host {
                DefaultHost::Zmx => HostName::Zmx,
                DefaultHost::Rex => HostName::Rex,
            },
        );
        if named.options.enabled {
            named
        } else {
            Self::named(settings, HostName::Zmx)
        }
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

    /// Whether a queued start from `origin` runs here; the daemon's worker
    /// has no terminal.
    fn takes_queued(&self, origin: Option<&Origin>) -> bool {
        self.options.enabled && self.options.opens_places && self.host.takes_queued(origin)
    }

    /// Whether the host runs Shoal for a workspace in the background.
    fn runs_in_background(&self) -> bool {
        self.options.opens_places && self.host.runs_in_background()
    }
}

/// A terminal host: a multiplexer or terminal app that runs Shoal work beside
/// the caller. A capability a host lacks keeps that work in this process.
pub(in crate::cli) trait Host: Sync {
    /// The name records, sessions and messages use.
    fn name(&self) -> HostName;

    /// Whether this host serves the terminal this process runs in, where
    /// config enables it.
    fn serves(&self) -> bool;

    /// Whether a queued start from `origin`, the place that queued it, runs
    /// here, where config lets the host open places.
    fn takes_queued(&self, _origin: Option<&Origin>) -> bool {
        false
    }

    /// Refuse when the programs this host runs are missing.
    fn ensure_installed(&self) -> Result<()> {
        Ok(())
    }

    /// The group labelled `label` on the server of `near`, or this
    /// machine's, created at `cwd` when there is none; `None` for a host
    /// without groups.
    fn group<'a>(
        &'a self,
        _near: Option<&'a Origin>,
        _label: &'a str,
        _cwd: &'a Path,
    ) -> BoxFuture<'a, Result<Option<Origin>>> {
        Box::pin(async { Ok(None) })
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

    /// Run `start` in a new background place, returning it where this host
    /// names its places to the daemon.
    fn run_in_background<'a>(
        &'a self,
        _start: &'a Start<'a>,
    ) -> BoxFuture<'a, Result<Option<Origin>>> {
        Box::pin(async { anyhow::bail!("{} cannot run in the background", self.name()) })
    }

    /// Run `argv` with `env` in `place`, an earlier place of this host, when
    /// it is idle, for the state directory `state`. Returns whether it ran.
    fn run_in_place<'a>(
        &'a self,
        _state: &'a Path,
        _place: &'a Origin,
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

    /// The place this process runs in, with what a later start needs to
    /// open a place beside it.
    fn origin(&self) -> Option<Origin> {
        None
    }

    /// The variables that name this host's place for a process in it, which
    /// a place another host opens must not inherit.
    fn place_variables(&self) -> &'static [&'static str] {
        &[]
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

    /// The session at `place`, where an agent or chat was recorded running,
    /// while it is open.
    fn session_at<'a>(&'a self, _place: &'a Origin) -> BoxFuture<'a, Result<Option<Session>>> {
        Box::pin(async { Ok(None) })
    }

    /// Whether the places recorded for agents of this host are sessions
    /// [`Host::session_at`] finds.
    fn attaches_places(&self) -> bool {
        false
    }

    /// Attach this terminal to `session`, one of [`Host::sessions`] or
    /// [`Host::session_at`], until the user detaches or the session ends.
    fn attach<'a>(&'a self, _session: &'a Session) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} keeps no sessions", self.name()) })
    }

    /// Start `demo` in a new background place of this host. Returns the
    /// place, or `None` when this host keeps no demos.
    fn start_demo<'a>(&'a self, _demo: &'a Demo<'a>) -> BoxFuture<'a, Result<Option<Origin>>> {
        Box::pin(async { Ok(None) })
    }

    /// Show the demo at `place` in this terminal until the user leaves it.
    fn open_demo<'a>(&'a self, _place: &'a Origin) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot show demos", self.name()) })
    }

    /// What the demo at `place` shows, oldest line first.
    fn demo_screen<'a>(&'a self, _place: &'a Origin) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { anyhow::bail!("{} cannot read a demo's screen", self.name()) })
    }

    /// Type `keys` into the demo at `place`.
    fn send_demo_keys<'a>(
        &'a self,
        _place: &'a Origin,
        _keys: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot type into a demo", self.name()) })
    }
}

/// Variables a demo's worker must not inherit from the agent that starts it:
/// the agent's execution marker would make the agent's exit stop the demo.
const NOT_INHERITED_BY_DEMOS: &[&str] = &[env::EXECUTION_ID];

/// The demo worker's argv for the place it runs in.
pub(in crate::cli) type DemoWorker<'a> =
    dyn Fn(&Origin) -> Result<Vec<OsString>> + Send + Sync + 'a;

/// Work a host starts in a new place, which generic code describes in full,
/// so the host only carries it out.
pub(in crate::cli) struct Start<'a> {
    /// The daemon state directory that owns the place.
    pub state: &'a Path,
    /// The place the start comes from, whose group the new place joins.
    pub origin: Option<&'a Origin>,
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
    pub launch: &'a str,
    pub state: &'a Path,
    pub origin: Option<&'a Origin>,
    pub workspace: &'a str,
    pub cwd: &'a Path,
    pub label: &'a str,
    pub env: &'a [(&'a str, String)],
    pub worker: &'a DemoWorker<'a>,
}

/// A terminal no host serves: everything runs in place.
struct Plain;

impl Host for Plain {
    fn name(&self) -> HostName {
        HostName::Terminal
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

    fn open_demo<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<()>> {
        Box::pin(background::open_demo(place.id()))
    }

    fn demo_screen<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { background::demo_screen(place.id()) })
    }

    fn send_demo_keys<'a>(
        &'a self,
        _place: &'a Origin,
        _keys: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async {
            anyhow::bail!(
                "a demo running in the background takes no keys; start it from zmx or Herdr"
            )
        })
    }
}

/// The host serving this terminal under `settings`. zmx claims a terminal
/// that no other host serves, which goes to the default host.
fn current(settings: &Effective) -> Configured {
    match Configured::all(settings).find(Configured::serves) {
        Some(host) if host.host.name() == HostName::Zmx => Configured::default_host(settings),
        Some(host) => host,
        None => Configured::plain(settings),
    }
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
    /// Where it was recorded, for a session found there.
    #[serde(skip)]
    place: Option<Origin>,
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
    if !host.options.opens_places {
        return Ok(false);
    }
    let cwd = std::env::current_dir()?;
    let repo = groups::Repo::Selected(plan.repository.clone());
    let origin = groups::beside(ctx, &host, &repo, &cwd).await?;
    let started = run_add(ctx, &host, origin.as_ref(), plan, name, placement).await?;
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
    let origin = queued_origin()?;
    let cwd = std::env::current_dir()?;
    let repo = groups::Repo::Selected(plan.repository.clone());
    groups::first_queued(
        ctx,
        settings,
        origin.as_ref(),
        &repo,
        &cwd,
        async |host, group| {
            naming::name_plan(ctx, host.options.name_template.clone(), plan, None).await?;
            run_add(ctx, host, group, plan, name, Placement::Background).await?;
            Ok(Some(()))
        },
    )
    .await?
    .context(NO_QUEUED_HOST)
}

const NO_QUEUED_HOST: &str = "no terminal host can start queued agents";

/// Have `host` run the add worker for `plan`, the planned workspace `name`,
/// in a new place beside `origin` as `placement` asks, returning whether it
/// left this process. The worker reads the plan from the place's
/// environment, and a place that keeps a shell gets the directive through
/// which the worker has it enter the workspace.
async fn run_add(
    ctx: &Context,
    host: &Configured,
    origin: Option<&Origin>,
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
        origin,
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

/// Refuse a background start that the daemon's worker could not run from
/// this terminal under `settings`, such as one whose host is not installed:
/// the host of this terminal where it takes queued starts, otherwise the
/// default host.
pub(in crate::cli) fn ensure_background(settings: &Effective) -> Result<()> {
    let origin = current_origin();
    Configured::all(settings)
        .find(|host| {
            host.host.name() == origin.as_ref().map_or(HostName::Terminal, |o| o.host)
                && host.takes_queued(origin.as_ref())
        })
        .unwrap_or_else(|| Configured::default_host(settings))
        .host
        .ensure_installed()
}

/// What a start the daemon runs later for this process needs, without a
/// terminal: the place it comes from and the output setting.
pub(crate) fn start_environment() -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([env::output_setting()]);
    if let Some(origin) = current_origin().and_then(|origin| serde_json::to_string(&origin).ok()) {
        env.insert(env::ORIGIN.to_owned(), origin);
    }
    env
}

/// The place a start the daemon runs for this worker comes from: the one
/// recorded with it, or for a start recorded before origins, the place its
/// host's variables name.
fn queued_origin() -> Result<Option<Origin>> {
    match std::env::var(env::ORIGIN) {
        Ok(origin) => Ok(Some(
            serde_json::from_str(&origin).context("invalid queued start origin")?,
        )),
        Err(_) => Ok(current_origin()),
    }
}

/// The variables naming places of hosts other than `host`, which a place
/// `host` opens must drop: Shoal there would take the caller's place for its
/// own, and record or close it.
fn foreign_place_variables(host: HostName) -> Vec<&'static str> {
    hosts()
        .filter(|other| other.name() != host)
        .flat_map(|other| other.place_variables().iter().copied())
        .collect()
}

/// The place this process runs in, as the host serving it names it.
pub(crate) fn current_origin() -> Option<Origin> {
    hosts().find_map(|host| host.origin())
}

/// Whether the place this process runs in closes once `workspace` is
/// removed, ending anything it streams.
pub(in crate::cli) fn closes_with(paths: &crate::paths::Paths, workspace: &str) -> bool {
    closing::closes_with(paths, workspace)
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
    let Some(place) = stopped_place(record).filter(|place| place.host == host.name()) else {
        return Ok(false);
    };
    let argv = shoal_command(ctx, args)?;
    host.run_in_place(&ctx.paths.state, &place, &argv, &place_env(ctx)?)
        .await
}

/// Where the agent of `record` stopped, unless that is where this process
/// runs, which is never reused.
fn stopped_place(record: &Path) -> Option<Origin> {
    let here = current_origin().and_then(|origin| origin.place);
    crate::execution::recovery::saved_place(record)
        .filter(|origin| origin.place.is_some() && origin.place != here)
}

/// The host named `name`, where config enables it.
fn enabled_host(settings: &Effective, name: HostName) -> Option<&'static dyn Host> {
    Configured::all(settings)
        .find(|host| host.options.enabled && host.host.name() == name)
        .map(|host| host.host)
}

/// Run Shoal with `args`, which restore the agent of `record` in
/// `workspace`, for the daemon's worker, which has no terminal: where the
/// agent stopped when that place is back at its prompt, otherwise in the
/// background of the first place a queued start from there can open.
pub(in crate::cli) async fn restore_queued(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    record: &Path,
    args: &[OsString],
) -> Result<()> {
    if let Some(host) = stopped_place(record).and_then(|place| enabled_host(settings, place.host))
        && rerun_in_host(ctx, host, record, args).await?
    {
        return Ok(());
    }
    let origin = queued_origin()?;
    let repo = groups::Repo::Id(&workspace.repository_id);
    groups::first_queued(
        ctx,
        settings,
        origin.as_ref(),
        &repo,
        &workspace.path,
        async |host, group| {
            start_in_background(ctx, host, group, workspace, args)
                .await
                .map(Some)
        },
    )
    .await?
    .context(NO_QUEUED_HOST)
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
    let repo = groups::Repo::Id(&workspace.repository_id);
    let origin = groups::beside(ctx, &host, &repo, &workspace.path).await?;
    start_in_background(ctx, &host, origin.as_ref(), workspace, args).await?;
    Ok(Some(host.host.background_places()))
}

/// Run Shoal with `args`, a restore of `workspace`, in a new background
/// place of `host` beside `origin`, telling the restore to close the place
/// with the workspace when config says so.
async fn start_in_background(
    ctx: &Context,
    host: &Configured,
    origin: Option<&Origin>,
    workspace: &Workspace,
    args: &[OsString],
) -> Result<()> {
    let template = host.options.name_template.clone();
    let label = naming::workspace_label(ctx, template, host.host, workspace).await;
    let mut argv = shoal_command(ctx, args)?;
    if host.closes_when_done {
        argv.extend(["--close-in".into(), host.host.name().as_str().into()]);
    }
    let start = Start {
        state: &ctx.paths.state,
        origin,
        cwd: &workspace.path,
        label: &label,
        focus: false,
        argv: &argv,
        env: &place_env(ctx)?,
    };
    if let Some(place) = host.host.run_in_background(&start).await? {
        record_owned(ctx, &workspace.id, place).await;
    }
    Ok(())
}

/// Record `place`, one Shoal created for `workspace`, so the daemon closes
/// it once the workspace is removed. Without the record the place stays
/// open, so a failure only warns.
async fn record_owned(ctx: &Context, workspace: &str, place: Origin) {
    if let Err(error) = super::client::record_owned_place(&ctx.paths, workspace, place).await {
        eprintln!("warning: cannot record the place Shoal opened: {error:#}");
    }
}

/// Record the place this process runs in, one the host named `host`
/// created for `workspace`, as [`record_owned`] does. Only that host names
/// it: another host's variables this process inherited name a place Shoal
/// did not create.
pub(in crate::cli) async fn record_owned_here(ctx: &Context, host: &str, workspace: &str) {
    if let Some(place) = named(host).and_then(|host| host.origin()) {
        record_owned(ctx, workspace, place).await;
    }
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
    let current = current(settings);
    let repo = groups::Repo::Id(&workspace.repository_id);
    let origin = groups::beside(ctx, &current, &repo, &workspace.path).await?;
    let Configured {
        host,
        closes_when_done,
        ..
    } = current;
    let worker = crate::cli::internal::internal_command(
        &ctx.paths,
        false,
        crate::cli::internal::Worker::Session {
            workspace: &workspace.id,
            agent,
            records,
            command,
            host: host.name().as_str(),
            close_in: closes_when_done.then(|| host.name().as_str()),
        },
    )?;
    let mut env = place_env(ctx)?;
    if let Some(entry) = env::queue_entry() {
        env.push((env::QUEUE_ENTRY, entry.to_owned()));
    }
    let start = Start {
        state: &ctx.paths.state,
        origin: origin.as_ref(),
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
    if let Err(error) = watch_here(ctx, host, workspace, false).await {
        eprintln!("warning: {error:#}");
    }
}

/// Close the place of the host named `host` that this process runs in, one
/// opened in the background for `workspace` before anything labelled it,
/// once the workspace is done. The place is labelled with the workspace
/// first, since the watcher only closes a place it finds hosting it; the
/// agent that starts there adds its own name.
pub(in crate::cli) async fn close_background_place_when_done(
    ctx: &Context,
    host: &str,
    workspace: &str,
) {
    if let Err(error) = watch_here(ctx, host, workspace, true).await {
        eprintln!("warning: {error:#}");
    }
}

/// Watch the place of the host named `host` that this process runs in,
/// labelling it with `workspace` first when `label`.
async fn watch_here(ctx: &Context, host: &str, workspace: &str, label: bool) -> Result<()> {
    let host = named(host).with_context(|| format!("unknown terminal host {host}"))?;
    let place = host
        .this_place()
        .with_context(|| format!("{} did not name the place this agent runs in", host.name()))?;
    if label {
        host.label_current(&ctx.paths.state, Owner::Workspace(workspace), "")
            .await?;
    }
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
    let repo = groups::Repo::Known(repository);
    let origin = groups::beside(ctx, &host, &repo, &repository.path).await?;
    let start = Start {
        state: &ctx.paths.state,
        origin: origin.as_ref(),
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

/// Every host's running agent sessions for `owner`, with those still open
/// at the `places` the daemon recorded for it.
pub(in crate::cli) async fn sessions(owner: Owner<'_>, places: &[&Origin]) -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    for host in hosts() {
        sessions.extend(host.sessions(owner).await?);
    }
    let mut seen = HashSet::new();
    for place in places {
        if seen.insert((place.host, place.id())) {
            sessions.extend(kept_by(place).session_at(place).await?);
        }
    }
    Ok(sessions)
}

/// The IDs of the owners of `kind` with at least one agent session in any
/// host, counting `places` recorded for owners by ID whose hosts attach them.
pub(in crate::cli) async fn session_owners(
    kind: OwnerKind,
    places: &[(&str, &Origin)],
) -> Result<HashSet<String>> {
    let mut owners = HashSet::new();
    for host in hosts() {
        owners.extend(host.session_owners(kind).await?);
    }
    owners.extend(
        places
            .iter()
            .filter(|(_, place)| kept_by(place).attaches_places())
            .map(|(owner, _)| (*owner).to_owned()),
    );
    Ok(owners)
}

/// Attach this terminal to `session` of `owner` through the host that keeps
/// it, then say how to return to it by `name` when it outlives the attach.
pub(in crate::cli) async fn attach(owner: Owner<'_>, name: &str, session: &Session) -> Result<i32> {
    let host = named(session.host).context("the session's host is unknown")?;
    host.attach(session).await?;
    report_detach(owner, name, &session.name).await?;
    Ok(0)
}

/// Say how to return to `owner`, named `name`, when its `session` still runs
/// after this terminal left it.
async fn report_detach(owner: Owner<'_>, name: &str, session: &str) -> Result<()> {
    if sessions(owner, &[])
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
/// the demo worker's argv for the place it runs in, which this returns.
pub(in crate::cli) async fn start_demo(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    name: &str,
    launch: &str,
    worker: &DemoWorker<'_>,
) -> Result<Origin> {
    let origin = current_origin();
    let repo = groups::Repo::Id(&workspace.repository_id);
    let env = place_env(ctx)?;
    let started = groups::first_queued(
        ctx,
        settings,
        origin.as_ref(),
        &repo,
        &workspace.path,
        async |host, group| {
            if host.host.ensure_installed().is_err() {
                return Ok(None);
            }
            let demo = Demo {
                launch,
                state: &ctx.paths.state,
                origin: group,
                workspace: &workspace.id,
                cwd: &workspace.path,
                label: &demo_label(host.host, workspace, name),
                env: &env,
                worker,
            };
            host.host.start_demo(&demo).await
        },
    )
    .await?;
    match started {
        Some(place) => Ok(place),
        None => background::start_demo(&ctx.paths, workspace, name, worker),
    }
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
    let place = &launch.origin;
    kept_by(place).open_demo(place).await
}

/// What the demo `launch` describes shows.
pub(in crate::cli) async fn demo_screen(launch: &Launch) -> Result<String> {
    let place = &launch.origin;
    kept_by(place).demo_screen(place).await
}

/// Type `keys` into the demo `launch` describes.
pub(in crate::cli) async fn send_demo_keys(launch: &Launch, keys: &str) -> Result<()> {
    let place = &launch.origin;
    kept_by(place).send_demo_keys(place, keys).await
}

/// The host, or the plain terminal, that keeps `place`.
fn kept_by(place: &Origin) -> &'static dyn Host {
    hosts()
        .chain([&Plain as &dyn Host])
        .find(|host| host.name() == place.host)
        .unwrap_or(&Plain)
}

/// The host, or the plain terminal, that goes by `name`.
fn named(name: &str) -> Option<&'static dyn Host> {
    hosts()
        .chain([&Plain as &dyn Host])
        .find(|host| host.name().as_str() == name)
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
            host: host.host.name().as_str(),
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
    match (&ctx.place, agent) {
        // A place is labelled before it is watched, since the watcher only
        // closes a place it finds hosting the workspace.
        (Some(place), _) => {
            place
                .host
                .label_current(
                    &ctx.paths.state,
                    Owner::Workspace(&workspace.id),
                    agent.unwrap_or_default(),
                )
                .await?;
            place.host.rename_place(&place.id, label).await;
            record_owned_here(ctx, place.host.name().as_str(), &workspace.id).await;
            if place.closes_when_done {
                closing::start_watcher(ctx, place.host, &place.id, &workspace.id).await?;
            }
            Ok(())
        }
        (None, Some(agent)) => {
            label_current(&ctx.paths, Owner::Workspace(&workspace.id), agent).await
        }
        (None, None) => Ok(()),
    }
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
