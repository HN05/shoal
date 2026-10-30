//! Starting Shoal work beside this terminal: additions, restores, tracked
//! agents and chats, focused or in the background, and the starts the
//! daemon runs later for a terminal that queued them.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use super::{
    add_place::add_worker_argv,
    groups,
    here::current_origin,
    host::{Attaches, Host, Places, Start},
    hosts::{Beside, Configured, beside, current, enabled_host, hosts, names_places},
    naming,
    sessions::{Owner, report_detach},
};
use crate::{
    cli::{
        commands::{issues::Issue, workspaces::AddPlan},
        context::Context,
    },
    config::Effective,
    env,
    model::{Repository, Workspace},
    sessions::Origin,
};

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
/// returning whether it left this process. A focused start stays here where
/// the host keeps agents in sessions this terminal attaches to, so its agent
/// starts in one. The worker reads the plan from the place's environment,
/// and a place that keeps a shell gets the directive through which the
/// worker has it enter the workspace.
async fn run_add(
    ctx: &Context,
    host: &Configured,
    places: &dyn Places,
    beside: Beside<'_>,
    plan: &AddPlan,
    name: &str,
    placement: Placement,
) -> Result<bool> {
    if placement == Placement::Focus && host.host.attaches().is_some() {
        return Ok(false);
    }
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
    let place = places.open(&start).await?;
    if let Some(directive) = directive {
        directive.hand_over()?;
    }
    if start.focus {
        show_opened(host, places, &place, start.label).await?;
    }
    Ok(true)
}

/// Show `place`, which `host` opened focused, in this terminal when the
/// host does not serve it; a terminal it serves shows its focused place.
async fn show_opened(
    host: &Configured,
    places: &dyn Places,
    place: &Origin,
    name: &str,
) -> Result<()> {
    if host.serves() {
        return Ok(());
    }
    places.show(place, name).await
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
pub(super) fn place_env(ctx: &Context) -> Result<Vec<(&'static str, String)>> {
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
/// this terminal under `settings`: one no host takes, or whose first host a
/// queued start from here would try is not installed.
pub(in crate::cli) fn ensure_background(settings: &Effective) -> Result<()> {
    groups::first_queued_host(settings, current_origin().as_ref())
        .context(NO_QUEUED_HOST)?
        .host
        .ensure_installed()
}

/// What a start the daemon runs later for this process needs, without a
/// terminal: the place it comes from, the hosts' servers this process
/// chooses, and the output setting.
pub(crate) fn start_environment() -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([env::output_setting()]);
    if let Some(origin) = current_origin().and_then(|origin| serde_json::to_string(&origin).ok()) {
        env.insert(env::ORIGIN.to_owned(), origin);
    }
    env.extend(server_selection(|name| std::env::var(name).ok()));
    env
}

/// The variables of a start that comes from no terminal, such as one an
/// app queues through the client API: no origin and every host's server
/// unset, so it opens in `default_host` whatever the daemon's own
/// environment names.
pub(crate) fn detached_start_environment() -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([env::output_setting()]);
    env.insert(env::ORIGIN.to_owned(), NO_ORIGIN.to_owned());
    env.extend(server_selection(|_| None));
    env
}

/// The recorded origin of a start that comes from no place.
const NO_ORIGIN: &str = "null";

/// The variables that choose a host's server, with the values `var` gives
/// them, so a start queued outside the host's places opens its place on
/// that server. An unset one is empty, which hosts read as unset, so the
/// daemon's own value does not stand in for it.
fn server_selection(
    var: impl Fn(&str) -> Option<String>,
) -> impl Iterator<Item = (String, String)> {
    hosts()
        .filter_map(Host::places)
        .flat_map(Places::server_variables)
        .map(move |name| ((*name).to_owned(), var(name).unwrap_or_default()))
}

/// The place a start the daemon runs for this worker comes from: the one
/// recorded with it, or for a start recorded before origins, the place its
/// host's variables name.
fn queued_origin() -> Result<Option<Origin>> {
    match std::env::var(env::ORIGIN) {
        Ok(origin) => recorded_origin(&origin),
        Err(_) => Ok(current_origin()),
    }
}

/// The origin a queued start recorded: a place, or none for a start from
/// no terminal.
fn recorded_origin(recorded: &str) -> Result<Option<Origin>> {
    serde_json::from_str(recorded).context("invalid queued start origin")
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
    places.open(&start).await.map(drop)
}

/// Where a tracked agent launched from this terminal runs apart from this
/// process.
enum AgentRun {
    /// In a new session this terminal attaches to.
    Session(&'static dyn Attaches),
    /// In a new place of a host that does not serve this terminal, shown
    /// here.
    Place(&'static dyn Places),
}

impl AgentRun {
    /// Where `host` runs the agent, or `None` for in place, as in a place
    /// of a host serving this terminal.
    fn of(host: &Configured) -> Option<Self> {
        if let Some(attaches) = host.host.attaches() {
            return Some(Self::Session(attaches));
        }
        host.places().filter(|_| !host.serves()).map(Self::Place)
    }
}

/// Run a tracked agent's `command` apart from this process, where the host
/// for this terminal runs agents so, and show it here. `None` means the
/// agent runs in place in this process. `records` are recovery records the
/// launch consumes once it starts.
pub(in crate::cli) async fn run_agent(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    agent: &str,
    records: &[PathBuf],
    command: &[OsString],
) -> Result<Option<i32>> {
    let current = current(settings);
    let Some(run) = AgentRun::of(&current) else {
        return Ok(None);
    };
    let repo = groups::Repo::Id(&workspace.repository_id);
    let origin = groups::beside(ctx, &current, &repo, &workspace.path).await?;
    let beside = current.beside(origin.as_ref());
    let worker = agent_worker(ctx, &current, workspace, agent, records, command)?;
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
    match run {
        AgentRun::Session(attaches) => {
            let owner = Owner::Workspace(&workspace.id);
            let session = attaches.run_agent(&start, owner, agent).await?;
            report_detach(attaches, &workspace.name, &session).await?;
        }
        AgentRun::Place(places) => {
            let place = places.open(&start).await?;
            places.show(&place, &workspace.name).await?;
        }
    }
    Ok(Some(0))
}

/// The worker that runs `agent`'s tracked `command` for `workspace` in a
/// place `host` opens, closing it with the workspace when config says so.
fn agent_worker(
    ctx: &Context,
    host: &Configured,
    workspace: &Workspace,
    agent: &str,
    records: &[PathBuf],
    command: &[OsString],
) -> Result<Vec<OsString>> {
    let name = host.host.name();
    crate::cli::internal::internal_command(
        &ctx.paths,
        false,
        crate::cli::internal::Worker::Session {
            workspace: &workspace.id,
            agent,
            records,
            command,
            host: name,
            close_in: host.closes_when_done.then_some(name),
        },
    )
}

/// Run Shoal with `args` for the chat `name` at the repository's checkout:
/// in a new session this terminal attaches to, as `run_agent` does for
/// tracked agents, otherwise in a new focused place, shown here when its
/// host does not serve this terminal. Returns whether it ran there; `false`
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
    if let Some(attaches) = host.host.attaches() {
        let session = attaches.run_chat(&start).await?;
        report_detach(attaches, name, &session).await?;
        return Ok(true);
    }
    let place = places.open(&start).await?;
    show_opened(&host, places, &place, name).await?;
    Ok(true)
}

/// Whether an agent launched here runs in a session this terminal attaches
/// to, leaving the caller's shell where it was once the terminal detaches.
pub(in crate::cli) fn keeps_agents_in_sessions(settings: &Effective) -> bool {
    current(settings).host.attaches().is_some()
}

/// The program an agent launch here needs besides the agent's own.
pub(in crate::cli) fn launcher_program() -> Option<&'static str> {
    hosts()
        .find_map(Host::attaches)
        .map(Attaches::launcher_program)
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
