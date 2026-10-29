//! Where Shoal runs work for the caller's terminal. Commands say what they
//! launch; this module chooses whether it takes the terminal or runs in the
//! background, and the [`Host`] that carries that out with its own
//! primitives. Commands never name a host; adding one means implementing
//! [`Host`] and the capability traits in [`host`] it supports, and listing
//! it in [`HOSTS`].
mod background;
mod closing;
mod groups;
mod herdr;
mod host;
mod naming;
mod rex;
mod typed;
mod zmx;

use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
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
use host::{ChatStart, Demo, DemoWorker, Host, Places, Prompts, Sessions, Start};
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
    /// Whether a place it opens from the caller's place splits that place
    /// instead.
    pub opens_splits: bool,
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
        opens_splits: false,
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
        self.options.enabled
            && self
                .places()
                .is_some_and(|places| places.takes_queued(origin))
    }

    /// How the host opens places, where config lets it.
    fn places(&self) -> Option<&'static dyn Places> {
        self.options
            .opens_places
            .then(|| self.host.places())
            .flatten()
    }

    /// Where a start from this terminal goes in `origin`, the place it runs
    /// in or the group it joins: splitting that place where config asks for
    /// splits and there is one, otherwise in a place of its own.
    fn beside<'a>(&self, origin: Option<&'a Origin>) -> Beside<'a> {
        Beside {
            origin,
            split: origin
                .and_then(|origin| origin.place.as_deref())
                .filter(|_| self.options.opens_splits),
        }
    }
}

/// Where a new place goes: into `origin`'s group, splitting `split` when
/// set.
#[derive(Clone, Copy)]
struct Beside<'a> {
    origin: Option<&'a Origin>,
    split: Option<&'a str>,
}

impl<'a> Beside<'a> {
    /// A place of its own in `origin`'s group, as a start without the
    /// caller's place opens.
    fn opening(origin: Option<&'a Origin>) -> Self {
        Self {
            origin,
            split: None,
        }
    }
}

/// Variables a demo's worker must not inherit from the agent that starts it:
/// the agent's execution marker would make the agent's exit stop the demo.
const NOT_INHERITED_BY_DEMOS: &[&str] = &[env::EXECUTION_ID];

/// A terminal no host serves: everything runs in place.
struct Plain;

impl Host for Plain {
    fn name(&self) -> HostName {
        HostName::Terminal
    }

    fn serves(&self) -> bool {
        true
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

/// What an agent session runs for, by ID; a host that keeps ownership in its
/// sessions labels them with it.
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

/// A running agent session that `shoal attach` can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub name: String,
    /// Terminals attached to the session.
    pub clients: u32,
    /// Where it was recorded, which names the host keeping it.
    #[serde(skip)]
    place: Origin,
}

impl Session {
    /// The name, and whether a terminal is attached.
    pub fn label(&self) -> String {
        let mut label = self.name.clone();
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
    let Some(places) = host.places() else {
        return Ok(false);
    };
    let cwd = std::env::current_dir()?;
    let repo = groups::Repo::Selected(plan.repository.clone());
    let origin = groups::beside(ctx, &host, &repo, &cwd).await?;
    let beside = host.beside(origin.as_ref());
    let started = run_add(ctx, &host, places, beside, plan, name, placement).await?;
    if started && placement == Placement::Background {
        outln!("{}", places.background_note(name));
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
            let Some(places) = host.places() else {
                return Ok(None);
            };
            naming::name_plan(ctx, host.options.name_template.clone(), plan, None).await?;
            let beside = Beside::opening(group);
            run_add(ctx, host, places, beside, plan, name, Placement::Background).await?;
            Ok(Some(()))
        },
    )
    .await?
    .context(NO_QUEUED_HOST)
}

const NO_QUEUED_HOST: &str = "no terminal host can start queued agents";

/// Have `host` run the add worker for `plan`, the planned workspace `name`,
/// in a new place of its `places` `beside` as `placement` asks,
/// returning whether it left this process. The worker reads the plan from
/// the place's environment, and a place that keeps a shell gets the
/// directive through which the worker has it enter the workspace.
async fn run_add(
    ctx: &Context,
    host: &Configured,
    places: &dyn Places,
    beside: Beside<'_>,
    plan: &AddPlan,
    name: &str,
    placement: Placement,
) -> Result<bool> {
    let worker = add_worker_argv(ctx, host)?;
    let directive = places
        .places_keep_shells()
        .then(ShellDirective::create)
        .transpose()?;
    let start = Start {
        state: &ctx.paths.state,
        origin: beside.origin,
        cwd: &std::env::current_dir()?,
        label: &add_label(host.host, plan, name),
        focus: placement == Placement::Focus,
        split: beside.split,
        argv: &worker,
        env: &add_env(ctx, plan, directive.as_ref())?,
    };
    let started = places.start_add(&start).await?;
    if let (true, Some(directive)) = (started, directive) {
        directive.hand_over()?;
    }
    Ok(started)
}

/// What the place `host` opens for `plan` is called: the plan's label where
/// the host shows names, otherwise the planned workspace `name`.
fn add_label(host: &dyn Host, plan: &AddPlan, name: &str) -> String {
    if names_places(host) {
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
/// the first host a queued start from here would try.
pub(in crate::cli) fn ensure_background(settings: &Effective) -> Result<()> {
    groups::first_queued_host(settings, current_origin().as_ref())
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
        .filter_map(Host::places)
        .flat_map(|other| other.place_variables().iter().copied())
        .collect()
}

/// The place this process runs in, as the host serving it names it.
pub(crate) fn current_origin() -> Option<Origin> {
    hosts().filter_map(Host::places).find_map(Places::origin)
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
    Ok(host
        .places()
        .filter(|_| reused)
        .map(Places::background_places))
}

/// Restore the agent of `record` in its earlier place in `host`, when idle.
async fn rerun_in_host(
    ctx: &Context,
    host: &dyn Host,
    record: &Path,
    args: &[OsString],
) -> Result<bool> {
    let Some(places) = host.places() else {
        return Ok(false);
    };
    let Some(place) = stopped_place(record).filter(|place| place.host == host.name()) else {
        return Ok(false);
    };
    let argv = shoal_command(ctx, args)?;
    places
        .run_in_place(&ctx.paths.state, &place, &argv, &place_env(ctx)?)
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
            let Some(places) = host.places() else {
                return Ok(None);
            };
            start_in_background(ctx, host, places, Beside::opening(group), workspace, args)
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
    let Some(places) = host.places() else {
        return Ok(None);
    };
    let repo = groups::Repo::Id(&workspace.repository_id);
    let origin = groups::beside(ctx, &host, &repo, &workspace.path).await?;
    let beside = host.beside(origin.as_ref());
    start_in_background(ctx, &host, places, beside, workspace, args).await?;
    Ok(Some(places.background_places()))
}

/// Run Shoal with `args`, a restore of `workspace`, in a new background
/// place of `host`'s `places` `beside`, telling the restore to close
/// the place with the workspace when config says so.
async fn start_in_background(
    ctx: &Context,
    host: &Configured,
    places: &dyn Places,
    beside: Beside<'_>,
    workspace: &Workspace,
    args: &[OsString],
) -> Result<()> {
    let template = host.options.name_template.clone();
    let label = naming::workspace_label(ctx, template, host.host, workspace).await;
    let name = host.host.name().as_str();
    // The restore records its place before anything can run there.
    let mut argv = shoal_command(ctx, args)?;
    argv.extend(["--opened-in".into(), name.into()]);
    if host.closes_when_done {
        argv.extend(["--close-in".into(), name.into()]);
    }
    let start = Start {
        state: &ctx.paths.state,
        origin: beside.origin,
        cwd: &workspace.path,
        label: &label,
        focus: false,
        split: beside.split,
        argv: &argv,
        env: &place_env(ctx)?,
    };
    places.run_in_background(&start).await
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
/// did not create. A host that reuses IDs tells this place apart from an
/// earlier one under its ID, whose record it replaces.
pub(in crate::cli) async fn record_owned_here(ctx: &Context, host: &str, workspace: &str) {
    let Some(host) = named(host) else {
        return;
    };
    let Some(mut place) = host.places().and_then(Places::origin) else {
        return;
    };
    if let Some(watches) = host.watches() {
        match watches.place_instance(place.id()).await {
            Ok(instance) => place.instance = instance,
            Err(error) => {
                eprintln!("warning: cannot record the place Shoal opened: {error:#}");
                return;
            }
        }
    }
    record_owned(ctx, workspace, place).await;
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
    let Some(sessions) = current.host.sessions() else {
        return Ok(None);
    };
    let repo = groups::Repo::Id(&workspace.repository_id);
    let origin = groups::beside(ctx, &current, &repo, &workspace.path).await?;
    let beside = current.beside(origin.as_ref());
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
        origin: beside.origin,
        cwd: &workspace.path,
        label: &workspace.name,
        focus: true,
        split: beside.split,
        argv: &worker,
        env: &env,
    };
    let owner = Owner::Workspace(&workspace.id);
    let Some(session) = sessions.run_agent(&start, owner, agent).await? else {
        return Ok(None);
    };
    report_detach(sessions, &workspace.name, &session).await?;
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
    let place = this_place(host)
        .with_context(|| format!("{} did not name the place this agent runs in", host.name()))?;
    if label && let Some(sessions) = host.sessions() {
        sessions
            .label_current(&ctx.paths.state, Owner::Workspace(workspace))
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
    let Some(places) = host.places() else {
        return Ok(false);
    };
    let repo = groups::Repo::Known(repository);
    let origin = groups::beside(ctx, &host, &repo, &repository.path).await?;
    let beside = host.beside(origin.as_ref());
    let start = Start {
        state: &ctx.paths.state,
        origin: beside.origin,
        cwd: &repository.path,
        label: name,
        focus: true,
        split: beside.split,
        argv: &shoal_command(ctx, args)?,
        env: &place_env(ctx)?,
    };
    let started = places.start_chat(&start).await?;
    match started {
        ChatStart::Tab => Ok(true),
        ChatStart::Session(session) => {
            if let Some(sessions) = host.host.sessions() {
                report_detach(sessions, name, &session).await?;
            }
            Ok(true)
        }
    }
}

/// The agent sessions still open at `places` in the hosts `settings`
/// enable, where the daemon recorded a workspace's executions or a chat
/// running, or the places Shoal created for a workspace.
pub(in crate::cli) async fn sessions(
    settings: &Effective,
    places: &[&Origin],
) -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    for place in distinct_places(places) {
        if let Some(host) = enabled_host(settings, place.host).and_then(Host::sessions) {
            sessions.extend(host.session_at(place).await?);
        }
    }
    Ok(sessions)
}

/// `places` without repeats of a place: the same ID in the same host and
/// its data, which tells servers of one host apart.
fn distinct_places<'a>(places: &[&'a Origin]) -> Vec<&'a Origin> {
    let mut seen = HashSet::new();
    places
        .iter()
        .copied()
        .filter(|place| seen.insert((place.host, place.id(), &place.data)))
        .collect()
}

/// The IDs of the owners with an agent session: those of `places`, recorded
/// for owners by ID, whose hosts attach them.
pub(in crate::cli) fn session_owners(places: &[(&str, &Origin)]) -> HashSet<String> {
    places
        .iter()
        .filter(|(_, place)| {
            kept_by(place)
                .sessions()
                .is_some_and(Sessions::attaches_places)
        })
        .map(|(owner, _)| (*owner).to_owned())
        .collect()
}

/// The IDs of the owners of `places`, recorded for owners by ID, with a
/// session still open there. A place whose host cannot say counts as closed.
pub(in crate::cli) async fn open_session_owners(places: &[(&str, &Origin)]) -> HashSet<String> {
    let mut owners = HashSet::new();
    for (owner, place) in places {
        if owners.contains(*owner) {
            continue;
        }
        if let Some(host) = kept_by(place).sessions()
            && let Ok(Some(_)) = host.session_at(place).await
        {
            owners.insert((*owner).to_owned());
        }
    }
    owners
}

/// Attach this terminal to `session` through the host that keeps it, then
/// say how to return to it by `name` when it outlives the attach.
pub(in crate::cli) async fn attach(name: &str, session: &Session) -> Result<i32> {
    let host = kept_by(&session.place);
    let sessions = host
        .sessions()
        .with_context(|| format!("{} keeps no sessions", host.name()))?;
    sessions.attach(session).await?;
    report_detach(sessions, name, &session.name).await?;
    Ok(0)
}

/// Say how to return to `session`, one `host` attached this terminal to, by
/// `name` when it still runs after this terminal left it.
async fn report_detach(host: &dyn Sessions, name: &str, session: &str) -> Result<()> {
    if host.left_running(session).await? {
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
            let Some(demos) = host.host.demos() else {
                return Ok(None);
            };
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
            demos.start_demo(&demo).await.map(Some)
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
    if names_places(host) {
        format!("{} demo {name}", workspace.name)
    } else {
        format!("{}-{name}", workspace.name)
    }
}

/// Show the demo `launch` describes in this terminal.
pub(in crate::cli) async fn open_demo(launch: &Launch) -> Result<()> {
    let place = &launch.origin;
    match kept_by(place).demos() {
        Some(demos) => demos.open_demo(place).await,
        None => background::open_demo(place.id()).await,
    }
}

/// What the demo `launch` describes shows.
pub(in crate::cli) async fn demo_screen(launch: &Launch) -> Result<String> {
    let place = &launch.origin;
    match kept_by(place).demos() {
        Some(demos) => demos.demo_screen(place).await,
        None => background::demo_screen(place.id()),
    }
}

/// Type `keys` into the demo `launch` describes.
pub(in crate::cli) async fn send_demo_keys(launch: &Launch, keys: &str) -> Result<()> {
    let place = &launch.origin;
    match kept_by(place).demos() {
        Some(demos) => demos.send_demo_keys(place, keys).await,
        None => background::send_demo_keys(),
    }
}

/// The place this process runs in, as `host` names the places it opens.
fn this_place(host: &dyn Host) -> Option<String> {
    host.places().and_then(Places::this_place)
}

/// Whether the places `host` opens show a name.
fn names_places(host: &dyn Host) -> bool {
    host.places().is_some_and(Places::names_places)
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
    /// The place as [`Places::this_place`] names it.
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
    let id = this_place(host)
        .with_context(|| format!("{} did not name the place this worker runs in", host.name()))?;
    ctx.place = Some(Place {
        host,
        id,
        closes_when_done,
    });
    execute_add(&ctx, plan).await
}

/// Tell the place this process runs in that it hosts `workspace`, whose
/// agent starts there when `runs_agent`: it takes `label`, and closes with the
/// workspace when it should. A session the user opened is labelled once an
/// agent starts in it.
pub(in crate::cli) async fn host_workspace(
    ctx: &Context,
    workspace: &Workspace,
    runs_agent: bool,
    label: &str,
) -> Result<()> {
    match (&ctx.place, runs_agent) {
        // A place is labelled before it is watched, since the watcher only
        // closes a place it finds hosting the workspace.
        (Some(place), _) => {
            if let Some(sessions) = place.host.sessions() {
                sessions
                    .label_current(&ctx.paths.state, Owner::Workspace(&workspace.id))
                    .await?;
            }
            if let Some(places) = place.host.places() {
                places.rename_place(&place.id, label).await;
            }
            record_owned_here(ctx, place.host.name().as_str(), &workspace.id).await;
            if place.closes_when_done {
                closing::start_watcher(ctx, place.host, &place.id, &workspace.id).await?;
            }
            Ok(())
        }
        (None, true) => label_current(&ctx.paths, Owner::Workspace(&workspace.id)).await,
        (None, false) => Ok(()),
    }
}

/// Whether the shell that started this process enters a new workspace
/// afterwards: the caller's own or that of the place opened for it.
pub(in crate::cli) fn follows_caller(ctx: &Context) -> bool {
    ctx.place
        .as_ref()
        .is_none_or(|place| place.host.places().is_none_or(Places::places_keep_shells))
}

/// Whether an agent launched here runs in a session this terminal attaches
/// to, leaving the caller's shell where it was once the terminal detaches.
pub(in crate::cli) fn keeps_agents_in_sessions(settings: &Effective) -> bool {
    current(settings)
        .host
        .sessions()
        .is_some_and(Sessions::keeps_agents_in_sessions)
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

/// Mark the session this process runs in, when it runs in one, as
/// `owner`'s, so cleanup closes it with `owner`.
pub(in crate::cli) async fn label_current(
    paths: &crate::paths::Paths,
    owner: Owner<'_>,
) -> Result<()> {
    for host in hosts().filter_map(Host::sessions) {
        host.label_current(&paths.state, owner).await?;
    }
    Ok(())
}

/// The program an agent launch here needs besides the agent's own.
pub(in crate::cli) fn launcher_program() -> Option<&'static str> {
    hosts()
        .filter_map(Host::sessions)
        .find_map(Sessions::launcher_program)
}

/// The session this process runs in, where an agent's wrapper can type a
/// prompt for it.
#[derive(Clone)]
pub(crate) struct PromptSession {
    host: &'static dyn Prompts,
    name: OsString,
}

impl PromptSession {
    pub(crate) fn current() -> Option<Self> {
        hosts()
            .filter_map(Host::prompts)
            .find_map(|host| host.prompt_session().map(|name| Self { host, name }))
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
