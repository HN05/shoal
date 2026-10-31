//! Agents Shoal launches by name. A launch runs the agent's configured
//! command with the arguments its provider describes (see [`crate::ai`]); an
//! [`Integration`] adds what configuration cannot say for a tool Shoal knows,
//! such as hooks, trust entries or a launch of its own. Adding an agent means
//! configuration, plus an integration its [`IntegrationKind`] selects when
//! the tool needs one; an integration may also add agents of its own
//! ([`ADDED`], behind its Cargo feature).
mod claude;
mod codex;
mod desktop;
mod detached;
#[cfg(feature = "happy")]
mod happy;
mod launch;
mod sessions;
mod shortcuts;
mod trust;

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::{
    agent::{Agent, AgentMode, ModelChoice},
    ai::IntegrationKind,
    cli::{client, commands::configured, context::Context, terminal, ui},
    config::{Config, Effective, named_commands::Commands},
    execution::recovery,
    fsutil,
    model::{Chat, RemovedWorkspace, Repository, Workspace},
    paths::Paths,
    protocol::{ConfigTarget, RepositoryMethod},
    tools::Tool,
};

pub(in crate::cli) use claude::APP as CLAUDE_APP;
pub(in crate::cli) use codex::APP as CODEX_APP;
pub(in crate::cli) use desktop::{App, T3};
pub(in crate::cli) use launch::input;
pub(crate) use sessions::session_sources;
pub(in crate::cli) use sessions::{SessionHome, install_session_files};
#[cfg(feature = "happy")]
pub(super) use shortcuts::happy;
pub(in crate::cli) use shortcuts::{Installed, happy_agents, shortcuts};
pub(super) use shortcuts::{open_app, with_app};

/// What an agent needs beyond its configured command and described
/// arguments. Each default adds nothing.
pub(in crate::cli) trait Integration: Sync {
    /// Arguments that register Shoal's hooks for the agent's session and,
    /// when `proxied`, what [`Self::route_through_proxy`] points at Shoal's
    /// proxy.
    fn hook_args(&self, _proxied: bool) -> std::io::Result<Vec<OsString>> {
        Ok(Vec::new())
    }

    /// Whether its launches can send their API requests to Shoal's proxy:
    /// those of an integration with arguments that point it there.
    fn routes_through_proxy(&self) -> bool {
        !self.proxy_args().is_empty()
    }

    /// Whether a response stream its proxy ends with a failure ends its
    /// turn, which the proxy then reports: for an agent with no hook of its
    /// own for a failed turn. One with a hook reports its turn's end itself,
    /// since a subagent's request may fail while the turn goes on.
    fn proxy_ends_turns(&self) -> bool {
        false
    }

    /// What its environment gets so its requests name its proxy's `key`:
    /// by default [`crate::proxy::KEY_VARIABLE`], which its proxy arguments
    /// name. `variable` reads the environment it runs with otherwise.
    fn proxy_environment(
        &self,
        key: &str,
        _variable: &dyn Fn(&str) -> Option<OsString>,
    ) -> Vec<(&'static str, OsString)> {
        vec![(crate::proxy::KEY_VARIABLE, key.into())]
    }

    /// Variables a launch through its proxy gets when the environment it
    /// runs with leaves them unset.
    fn proxy_defaults(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// Arguments that only send its API requests to Shoal's proxy, for a
    /// launch Shoal gives no hooks, such as a configured resume command,
    /// with the placeholders of [`Self::hook_args`].
    fn proxy_args(&self) -> Vec<OsString> {
        Vec::new()
    }

    /// Point `args` at the proxy by `routing`, for the agent running with
    /// `environment`; nothing for an integration that does not route.
    fn route_through_proxy(
        &self,
        _args: &mut Vec<OsString>,
        _routing: &Routing,
        _environment: &AgentEnvironment,
    ) -> Result<()> {
        Ok(())
    }

    /// The hooks of a chat, which belongs to no workspace and so reports no
    /// turn state. They inherit the chat's identity.
    fn chat_hook_args(&self) -> std::io::Result<Vec<OsString>> {
        Ok(Vec::new())
    }

    /// The config file in which the agent records trusted directories.
    fn trust_file(&self) -> Option<&'static trust::TrustFile> {
        None
    }

    /// How the idle agent wakes for an update.
    fn wakes(&self) -> Wake {
        Wake::Never
    }

    /// The key that cancels the agent's running turn in its terminal and
    /// leaves it waiting for the next message, unless Shoal knows none.
    fn interrupt_key(&self) -> Option<&'static str> {
        None
    }

    /// The programs a launch starts, when not its configured command's.
    fn programs(&self) -> Option<Vec<&'static str>> {
        None
    }

    /// How long the agent's prompt cache lives after a turn, as its own
    /// settings give it; `variable` reads the environment it runs with.
    fn prompt_cache(
        &self,
        _variable: &dyn Fn(&str) -> Option<OsString>,
        _home: &Path,
    ) -> Option<std::time::Duration> {
        None
    }

    /// The failure that `event`, from its hook for a turn an API error ended,
    /// reports; `None` for an agent without one.
    fn turn_failure(&self, _event: &serde_json::Value) -> Option<crate::state::TurnFailure> {
        None
    }

    /// The app that runs the agent's workspace sessions and chats detached
    /// from any terminal, in place of the tracked terminal launch and the
    /// chat run in place.
    fn detached(&self) -> Option<&dyn detached::App> {
        None
    }

    /// The desktop app `default_mode = "app"` and `--app` hand the agent's
    /// workspace launches to.
    fn app(&self) -> Option<&'static desktop::App> {
        None
    }

    /// The directory the agent keeps its sessions' files in, running in
    /// `environment`; `None` for an agent whose sessions cannot move to
    /// another machine.
    fn session_dir(&self, _environment: &AgentEnvironment) -> Result<Option<PathBuf>> {
        Ok(None)
    }

    /// The files of session `id` in `directory`, which [`Self::session_dir`]
    /// gave, relative to it.
    fn session_files(&self, _directory: &Path, _id: &str) -> std::io::Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
}

/// How an idle agent wakes for an update, which decides whether updates are
/// pushed to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Wake {
    /// It does not; it finds updates by watching for them.
    Never,
    /// A hook of its own waits for the update and starts its next turn.
    Hook,
    /// Its wrapper types a prompt into its session, for an agent without a
    /// hook that can start its turn; only a session the wrapper can type
    /// into wakes it.
    Prompt,
}

/// What config sets for an integration, from the section of its own it may
/// have.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::cli) struct Options {
    /// Why config turns the integration off, if it does; pickers and
    /// completion leave its agents out.
    pub off: Option<&'static str>,
    /// The desktop app config hands the agent's launches to.
    pub app: Option<&'static desktop::App>,
}

/// An agent without an integration: its configuration says everything.
struct Configured;

impl Integration for Configured {}

fn integration(kind: IntegrationKind) -> &'static dyn Integration {
    match kind {
        IntegrationKind::Claude => &claude::Claude,
        IntegrationKind::Codex => &codex::Codex,
        IntegrationKind::None => &Configured,
    }
}

/// What `settings` set for agent `name`, which runs with the integration of
/// `kind`, from its own `[agents.<name>]`: `default_mode = "app"` hands
/// launches to the integration's desktop app.
fn tool_options(name: &str, kind: IntegrationKind, settings: &Effective) -> Options {
    let mode = settings.agents.get(name).map(|agent| agent.default_mode);
    Options {
        app: integration(kind)
            .app()
            .filter(|_| mode == Some(AgentMode::App)),
        ..Options::default()
    }
}

/// An agent an integration adds besides the AI tools, with the integration
/// that runs it and the options `settings` give that integration.
struct Added {
    name: &'static str,
    integration: &'static dyn Integration,
    options: fn(&Effective) -> Options,
}

/// Agents integrations add, built only with each integration's Cargo
/// feature.
#[cfg(feature = "happy")]
const ADDED: &[Added] = &[
    Added {
        name: "happy-claude",
        integration: &happy::CLAUDE,
        options: |settings| happy::options(&settings.terminal.happy),
    },
    Added {
        name: "happy-codex",
        integration: &happy::CODEX,
        options: |settings| happy::options(&settings.terminal.happy),
    },
];
#[cfg(not(feature = "happy"))]
const ADDED: &[Added] = &[];

/// Where a chat of agent `name` runs when an integration it adds starts it
/// outside any terminal, such as an app.
pub(in crate::cli) fn chat_place(name: &str) -> Option<&'static str> {
    let app = added(name).and_then(|added| added.integration.detached());
    app.map(detached::App::place)
}

/// The agent `name`, when an integration adds it.
fn added(name: &str) -> Option<&'static Added> {
    ADDED.iter().find(|added| added.name == name)
}

/// The integration agent `name` runs with under `settings`: its AI tool's,
/// else that of the integration adding it.
fn agent_integration(settings: &Effective, name: &str) -> &'static dyn Integration {
    match settings.integration(name) {
        IntegrationKind::None => added(name).map_or(&Configured, |added| added.integration),
        kind => integration(kind),
    }
}

/// What `settings` set for the integration agent `name` runs with.
fn agent_options(settings: &Effective, name: &str) -> Options {
    match (settings.integration(name), added(name)) {
        (IntegrationKind::None, Some(added)) => (added.options)(settings),
        (kind, _) => tool_options(name, kind, settings),
    }
}

/// The desktop app a launch with `options` opens: config hands launches to
/// it, the app cannot take a first message, so a `prompted` launch stays
/// in the terminal, and a `tracked` launch is never handed to an app.
fn opens_app(options: Options, prompted: bool, tracked: bool) -> Option<&'static desktop::App> {
    options.app.filter(|_| !prompted && !tracked)
}

/// One launch of an agent in a ready workspace; settings are never cached
/// across launches.
pub(in crate::cli) struct Launch {
    pub name: String,
    pub workspace: Workspace,
    pub settings: Effective,
    /// The first message, such as an issue prompt.
    pub prompt: Option<String>,
    pub args: Vec<OsString>,
    /// The arguments that start the agent from a fork of a session, which
    /// lead the others as a subcommand does.
    pub fork: Vec<OsString>,
    /// The model and effort it runs with.
    pub model: ModelChoice,
    /// Recovery records of stopped agents this one replaces, consumed once
    /// it has started.
    pub replaces: Vec<PathBuf>,
}

/// What a workspace's agent runs with, which says where its tool keeps its
/// files: this process's environment with `configured`, the workspace's
/// `[env]` for a tracked agent.
pub(in crate::cli) struct AgentEnvironment<'a> {
    pub home: &'a Path,
    pub configured: &'a BTreeMap<String, OsString>,
}

impl<'a> AgentEnvironment<'a> {
    /// This process's environment alone, which agents Shoal does not track,
    /// such as chats and desktop apps, run with.
    pub fn inherited(home: &'a Path) -> Self {
        static NONE: BTreeMap<String, OsString> = BTreeMap::new();
        Self {
            home,
            configured: &NONE,
        }
    }
}

/// One chat about a repository in its registered checkout.
pub(in crate::cli) struct ChatLaunch {
    pub repository: Repository,
    /// The name the chat is recorded under.
    pub name: String,
    pub settings: Effective,
    /// The rendered chat template.
    pub instructions: String,
    /// The task the chat starts on before the user speaks, if any.
    pub opening: Option<String>,
    /// Other working directories the chat's task needs, beside its own.
    pub directories: Vec<PathBuf>,
    /// The model and effort it runs with.
    pub model: ModelChoice,
    pub args: Vec<OsString>,
}

/// How a launch relates to the agents around it.
#[derive(Default)]
pub(in crate::cli) struct LaunchOptions {
    /// Whether the agent must run tracked, never handed to a desktop app, as
    /// a detached start must.
    pub tracked: bool,
    /// As [`Launch::replaces`].
    pub replaces: Vec<PathBuf>,
    /// The session the agent starts from a fork of, which stays unchanged.
    pub fork: Option<String>,
    /// The model and effort the launch asks for.
    pub model: ModelRequest,
}

/// The model and effort a launch asks for.
pub(in crate::cli) enum ModelRequest {
    /// Values over the configured ones, which fill in the rest.
    Override(ModelChoice),
    /// What a restored agent ran with, used as it is; a value it left to
    /// the tool stays the tool's.
    Kept(ModelChoice),
}

impl Default for ModelRequest {
    fn default() -> Self {
        Self::Override(ModelChoice::default())
    }
}

/// Whether a launch of `agent` under `settings` runs in a session this
/// terminal attaches to, rather than in place or the agent's own way.
pub(in crate::cli) fn runs_in_session(
    agent: &Agent,
    settings: &Effective,
    prompted: bool,
    tracked: bool,
) -> bool {
    let name = agent.name();
    let opens_app = opens_app(agent_options(settings, name), prompted, tracked);
    opens_app.is_none()
        && agent_integration(settings, name).detached().is_none()
        && terminal::keeps_agents_in_sessions(settings)
}

/// Start an agent in a ready workspace, giving it the prompt the way it
/// accepts one.
pub(super) async fn launch_agent(
    ctx: &Context,
    agent: Agent,
    workspace: Workspace,
    prompt: Option<String>,
    args: Vec<OsString>,
    options: LaunchOptions,
) -> Result<i32> {
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let integration = agent_integration(&settings, agent.name());
    let fork = match &options.fork {
        Some(session) => fork_args(&settings, agent.name(), session, &workspace.path)?,
        None => Vec::new(),
    };
    let app = opens_app(
        agent_options(&settings, agent.name()),
        prompt.is_some(),
        options.tracked,
    );
    let model = match options.model {
        ModelRequest::Override(requested) => resolve_model(&settings, agent.name(), requested),
        ModelRequest::Kept(model) => model,
    };
    let launch = Launch {
        name: agent.into(),
        workspace,
        settings,
        prompt,
        args,
        fork,
        model,
        replaces: options.replaces,
    };
    let replaces = launch.replaces.clone();
    let code = match (app, integration.detached()) {
        (Some(app), _) => {
            warn_app_model(app, &launch.model);
            let trust = integration.trust_file();
            launch::open_app(ctx, &launch.workspace, app, trust, launch.args).await?
        }
        (None, Some(detached)) => detached::start(ctx, integration, detached, launch).await?,
        (None, None) => return launch::tracked(ctx, integration, launch).await,
    };
    // A launch of its own returns once its agent has started.
    for record in &replaces {
        crate::execution::recovery::consume(record)?;
    }
    Ok(code)
}

/// Say that desktop `app` opens with its own model and effort, when the
/// launch it takes over would have run with `model`.
fn warn_app_model(app: &desktop::App, model: &ModelChoice) {
    if !model.is_empty() {
        eprintln!(
            "warning: {} opens with its own model and effort; Shoal cannot pass the \
             configured ones to it",
            app.name
        );
    }
}

/// The arguments that run agent `name` with `model`, refusing an agent
/// without the template for a value it sets.
pub(crate) fn model_args(
    settings: &Effective,
    name: &str,
    model: &ModelChoice,
) -> Result<Vec<OsString>> {
    settings
        .launch(name)
        .arguments
        .model_args(model)
        .map_err(|template| missing_template(name, template))
}

/// The model and effort a launch of agent `name` runs with, as
/// [`Effective::model`] resolves them, with what it leaves to the tool taken
/// from the configuration of the agent an app it runs in starts.
pub(in crate::cli) fn resolve_model(
    settings: &Effective,
    name: &str,
    requested: ModelChoice,
) -> ModelChoice {
    let model = settings.model(name, requested);
    let started = agent_integration(settings, name)
        .detached()
        .and_then(|app| settings.tools.get(app.agent()));
    match started {
        Some(tool) => model.or(tool.model.clone()),
        None => model,
    }
}

/// Refuse agent `name` with `model` before anything starts when its launch
/// cannot pass the values on: through its own templates, or those of the
/// agent an app it runs in starts.
pub(in crate::cli) fn check_model(
    settings: &Effective,
    name: &str,
    model: &ModelChoice,
) -> Result<()> {
    match agent_integration(settings, name).detached() {
        Some(app) => detached::model_args(app, settings, name, model).map(drop),
        None => model_args(settings, name, model).map(drop),
    }
}

/// The refusal of a model or effort for agent `name`, which lacks `template`.
fn missing_template(name: &str, template: &str) -> anyhow::Error {
    let option = template.trim_end_matches("_args");
    anyhow::anyhow!(
        "cannot start {name} with a configured or requested {option}: set [agents.{name}] {template}"
    )
}

/// How `shoal <name>` runs.
enum NamedRun {
    /// A configured command.
    Command,
    /// An agent of the targeted workspace, or of this machine's config.
    Agent,
    /// An agent only these workspaces define, outside every workspace.
    Elsewhere(Vec<crate::model::Workspace>),
}

/// Whether `shoal <name>` launches an agent rather than a configured command.
pub(super) async fn is_agent(ctx: &Context, name: &str, workspace: Option<&str>) -> Result<bool> {
    Ok(!matches!(
        classify(ctx, name, workspace).await?,
        NamedRun::Command
    ))
}

/// Agents are those the targeted workspace's settings resolve, else this
/// machine's config; outside every workspace, also a repository's own,
/// which runs in a workspace defining it, as a repository's command does.
async fn classify(ctx: &Context, name: &str, workspace: Option<&str>) -> Result<NamedRun> {
    if added(name).is_some() {
        return Ok(NamedRun::Agent);
    }
    let target = agent_target(ctx, workspace).await?;
    // A named workspace, resolved or not, never falls back to another.
    let in_workspace = target.is_some() || workspace.is_some();
    // A workspace that does not resolve is the command's own error to report.
    let settings = match target {
        Some(target) => client::settings(&ctx.paths, target).await.ok(),
        None => None,
    };
    let tools = match settings {
        Some(settings) => settings.tools,
        None => Config::load(&ctx.paths)?.tools()?,
    };
    if crate::ai::agents(&tools).any(|agent| agent == name) {
        return Ok(NamedRun::Agent);
    }
    if in_workspace {
        return Ok(NamedRun::Command);
    }
    let defining = defining_workspaces(ctx, name).await;
    Ok(if defining.is_empty() {
        NamedRun::Command
    } else {
        NamedRun::Elsewhere(defining)
    })
}

/// The workspaces whose settings resolve `name` as an agent.
async fn defining_workspaces(ctx: &Context, name: &str) -> Vec<crate::model::Workspace> {
    let Ok(workspaces) = client::workspaces(&ctx.paths).await else {
        return Vec::new();
    };
    let mut defining = Vec::new();
    for workspace in workspaces {
        let target = ConfigTarget::Workspace(workspace.id.clone());
        // A workspace whose configuration cannot load has no agents.
        if let Ok(settings) = client::settings(&ctx.paths, target).await
            && crate::ai::agents(&settings.tools).any(|agent| agent == name)
        {
            defining.push(workspace);
        }
    }
    defining
}

/// The workspace `workspace` names, else the one running a command would
/// select: the current directory's, then the caller's scope.
async fn agent_target(ctx: &Context, workspace: Option<&str>) -> Result<Option<ConfigTarget>> {
    if let Some(workspace) = workspace {
        // A selector that does not resolve is the command's own error.
        let workspace = crate::cli::ui::explicit_workspace(ctx, workspace.to_owned()).await;
        return Ok(workspace.ok().map(ConfigTarget::Workspace));
    }
    // Without a daemon there is no workspace context, only the machine's.
    let Ok(workspaces) = client::workspaces(&ctx.paths).await else {
        return Ok(None);
    };
    let current = configured::current_workspace(&workspaces)?;
    Ok(current.map(|workspace| ConfigTarget::Workspace(workspace.id.clone())))
}

/// `shoal <name>` and `shoal run <name>`: agents launch with their prompts,
/// other names run as plain configured commands.
pub(super) async fn run_named(
    ctx: &Context,
    name: &str,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = match classify(ctx, name, workspace.as_deref()).await? {
        NamedRun::Command => return configured::run(ctx, name, workspace, args).await,
        NamedRun::Agent => workspace,
        NamedRun::Elsewhere(defining) => {
            ensure!(
                ctx.interactive(),
                "{name} is an agent only some repositories define; run it in one of their \
                 workspaces or name one with --workspace"
            );
            Some(ui::pick_workspace(ctx, defining, "workspace").await?)
        }
    };
    launch::shortcut(
        ctx,
        name.parse()?,
        workspace,
        None,
        args,
        LaunchOptions::default(),
    )
    .await
}

/// The requested agent, else the configured `agent.default`, else a picker.
pub(super) async fn default_agent(
    ctx: &Context,
    target: ConfigTarget,
    agent: Option<Agent>,
) -> Result<Option<Agent>> {
    let settings = client::settings(&ctx.paths, target).await?;
    match agent.or(settings.agent.default.clone()) {
        Some(agent) => {
            ensure_available(agent.name(), &settings)?;
            Ok(Some(agent))
        }
        None => pick_default_agent(ctx, &settings),
    }
}

/// The picker standing in for a missing `agent.default`.
pub(super) fn pick_default_agent(ctx: &Context, settings: &Effective) -> Result<Option<Agent>> {
    ensure!(
        ctx.interactive(),
        "no agent selected; pass --agent or set agent.default in the repository or global config"
    );
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let mut choices: Vec<_> = names(settings)
        .into_iter()
        .filter(|name| installed(name, &settings.launchers, &search_path))
        .map(|name| (name.parse().ok(), name))
        .collect();
    // A picker cannot run without fzf. Treat that as “No agent” so callers
    // without the optional interactive dependency retain the existing flow.
    if choices.is_empty()
        || fsutil::find_executable(OsStr::new(Tool::Fzf.program()), &search_path).is_none()
    {
        return Ok(None);
    }
    choices.push((None, "No agent".into()));
    let choices: Vec<_> = choices
        .iter()
        .map(|(agent, label)| (agent.clone(), label.as_str()))
        .collect();
    ui::pick_choice(ctx, "Agent> ", &choices)
}

/// The agents pickers and completion offer under `settings`, built-in
/// providers first.
pub(in crate::cli) fn names(settings: &Effective) -> Vec<String> {
    let mut names: Vec<String> = crate::ai::PROVIDERS
        .iter()
        .map(|provider| provider.name.to_owned())
        .collect();
    for name in crate::ai::agents(&settings.tools) {
        if !names.iter().any(|known| known == name) {
            names.push(name.to_owned());
        }
    }
    names.extend(
        ADDED
            .iter()
            .filter(|added| {
                !names.iter().any(|known| known == added.name)
                    && (added.options)(settings).off.is_none()
            })
            .map(|added| added.name.to_owned())
            .collect::<Vec<_>>(),
    );
    names
}

/// Agent names Shoal knows without configuration, for help and completion.
pub(in crate::cli) fn builtin_names() -> Vec<String> {
    let mut names: Vec<String> = crate::ai::PROVIDERS
        .iter()
        .map(|provider| provider.name.to_owned())
        .collect();
    for Added { name, .. } in ADDED {
        if !names.iter().any(|known| known == name) {
            names.push((*name).to_owned());
        }
    }
    names
}

/// Whether every executable the agent starts is available, so pickers and
/// completion offer only agents that can launch.
pub(in crate::cli) fn installed(name: &str, commands: &Commands, search_path: &OsStr) -> bool {
    programs(name, commands).is_some_and(|programs| {
        programs
            .into_iter()
            .all(|program| program_available(program, search_path))
    })
}

/// Refuse an agent that cannot run under `settings` before anything starts.
pub(crate) fn ensure_available(name: &str, settings: &Effective) -> Result<()> {
    if let Some(reason) = agent_options(settings, name).off {
        anyhow::bail!("cannot start {name}: {reason}");
    }
    launch_proxy(settings, name)?;
    ensure_installed(name, &settings.launchers)
}

/// Refuse agent `name` when it cannot start from a fork of a session: it
/// has no arguments for one, or an app runs it apart from any terminal.
pub(in crate::cli) fn ensure_forks(settings: &Effective, name: &str) -> Result<()> {
    if let Some(app) = agent_integration(settings, name).detached() {
        anyhow::bail!(
            "cannot fork a session of {name}: {} cannot start one from a fork",
            app.name()
        );
    }
    ensure!(
        settings.launch(name).arguments.fork.is_some(),
        "cannot fork a session of {name}: configure [agents.{name}] fork"
    );
    Ok(())
}

/// The arguments that start agent `name` in `directory` from a fork of
/// `session`, refusing an agent [`ensure_forks`] refuses.
fn fork_args(
    settings: &Effective,
    name: &str,
    session: &str,
    directory: &Path,
) -> Result<Vec<OsString>> {
    ensure_forks(settings, name)?;
    let directory = directory
        .to_str()
        .context("cannot fork a session in a directory whose path is not UTF-8")?;
    Ok(settings
        .launch(name)
        .arguments
        .fork_args(session, directory)
        .unwrap_or_default())
}

/// The proxy agent `name`'s API requests go through, if it names one; an
/// error when Shoal cannot point it there, for every launch, a resumed one
/// included.
pub(crate) fn launch_proxy<'a>(
    settings: &'a Effective,
    name: &str,
) -> Result<Option<&'a crate::proxy::Proxy>> {
    let Some(proxy) = settings.proxy(name) else {
        return Ok(None);
    };
    ensure!(
        integration(settings.integration(name)).routes_through_proxy(),
        "cannot start {name}: Shoal cannot send its API requests through a proxy; \
         remove proxy from its [machine.agents.{name}]"
    );
    Ok(Some(proxy))
}

/// Refuse an agent whose executables are missing.
fn ensure_installed(name: &str, commands: &Commands) -> Result<()> {
    let programs = programs(name, commands).with_context(|| {
        format!("unknown agent {name:?}; set [agents.{name}] command in Shoal config")
    })?;
    for program in programs {
        ensure_program(name, program)?;
    }
    Ok(())
}

/// Refuse a launcher whose executable is not installed.
pub(in crate::cli) fn ensure_program(name: &str, program: &str) -> Result<()> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    ensure!(
        program_available(program, &search_path),
        "cannot start {name}: {program} is not installed or not on PATH"
    );
    Ok(())
}

/// The executables an agent starts; `None` when it has no launcher. Terminal
/// agents launched on a terminal also start its session host.
fn programs<'a>(name: &str, commands: &'a Commands) -> Option<Vec<&'a str>> {
    if let Some(programs) = added(name).and_then(|added| added.integration.programs()) {
        return Some(programs);
    }
    commands
        .get(name)
        .and_then(|argv| argv.first())
        .map(|program| {
            let mut programs = vec![program.as_str()];
            programs.extend(super::terminal::launcher_program());
            programs
        })
}

/// Programs with placeholders or workspace-relative paths resolve only at
/// launch, so they are assumed available.
pub(in crate::cli) fn program_available(program: &str, search_path: &OsStr) -> bool {
    let path = Path::new(program);
    if path.is_absolute() {
        fsutil::is_executable(path).unwrap_or(false)
    } else if program.contains('/') || program.contains('{') {
        true
    } else {
        fsutil::find_executable(OsStr::new(program), search_path).is_some()
    }
}

/// Arguments that register Shoal's hooks for the session of an agent with
/// integration `kind`, and when `proxied` route its API requests through
/// Shoal's proxy.
pub(crate) fn hook_args(kind: IntegrationKind, proxied: bool) -> std::io::Result<Vec<OsString>> {
    integration(kind).hook_args(proxied)
}

/// Whether a failure stream from the proxy of an agent with integration
/// `kind` ends its turn.
pub(crate) fn proxy_ends_turns(kind: IntegrationKind) -> bool {
    integration(kind).proxy_ends_turns()
}

/// What the environment of an agent with integration `kind` gets so its
/// requests name its proxy's `key`, reading the environment it runs with
/// otherwise from `variable`.
pub(crate) fn proxy_environment(
    kind: IntegrationKind,
    key: &str,
    variable: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<(&'static str, OsString)> {
    integration(kind).proxy_environment(key, variable)
}

/// The variables an agent with integration `kind` gets through its proxy
/// that `variable`, the environment it runs with otherwise, leaves unset.
pub(crate) fn proxy_defaults(
    kind: IntegrationKind,
    variable: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<(&'static str, &'static str)> {
    integration(kind)
        .proxy_defaults()
        .iter()
        .copied()
        .filter(|(name, _)| variable(name).is_none())
        .collect()
}

/// How long the prompt cache of an agent with integration `kind` lives after
/// a turn, as its own settings in the environment `variable` reads give it.
pub(crate) fn prompt_cache(
    kind: IntegrationKind,
    variable: &dyn Fn(&str) -> Option<OsString>,
    home: &Path,
) -> Option<std::time::Duration> {
    integration(kind).prompt_cache(variable, home)
}

/// Whether an agent with integration `kind` can go through Shoal's proxy.
pub(crate) fn routes_through_proxy(kind: IntegrationKind) -> bool {
    integration(kind).routes_through_proxy()
}

/// Where a proxied agent's launch goes: its proxy's `url`, the `directory`
/// it runs in, and a `private` directory only its user can read, for what
/// must stay off its command line, which any local user can read.
pub(crate) struct Routing<'a> {
    pub url: &'a str,
    pub directory: &'a Path,
    pub private: &'a Path,
}

/// `command` of an agent with integration `kind`, running with `configured`
/// added to this process's environment, sent to its proxy by `routing`.
pub(crate) fn route_through_proxy(
    kind: IntegrationKind,
    mut command: Vec<OsString>,
    routing: &Routing,
    home: &Path,
    configured: &BTreeMap<String, OsString>,
) -> Result<Vec<OsString>> {
    let environment = AgentEnvironment { home, configured };
    integration(kind).route_through_proxy(&mut command, routing, &environment)?;
    Ok(command)
}

/// `command`, a configured command of agent `name` that Shoal gives no
/// hooks, with the arguments that send its API requests to its proxy when it
/// has one it can use.
pub(crate) fn with_proxy_args(settings: &Effective, name: &str, command: &mut Vec<OsString>) {
    let kind = settings.integration(name);
    if settings.proxy(name).is_none() || !routes_through_proxy(kind) {
        return;
    }
    insert_options(command, integration(kind).proxy_args());
}

/// Add `options` to `command`, a configured command: before a `--`, after
/// which arguments are the agent's prompt, else at the end.
pub(crate) fn insert_options(command: &mut Vec<OsString>, options: Vec<OsString>) {
    let at = command
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(command.len());
    command.splice(at..at, options);
}

/// What a workspace's agent `name` runs with besides its instructions, prompt
/// and forwarded arguments: Shoal's hooks and [`workspace_dir_args`].
pub(crate) async fn workspace_args(
    paths: &crate::paths::Paths,
    settings: &Effective,
    name: &str,
    workspace: &Workspace,
) -> Result<Vec<OsString>> {
    let proxied = settings.proxy(name).is_some();
    let mut args = hook_args(settings.integration(name), proxied)?;
    args.extend(workspace_dir_args(paths, settings, name, workspace).await);
    Ok(args)
}

/// The arguments that give a workspace's agent `name` the configured agent
/// directories and the repository's workspaces directory as other working
/// directories, so it may also work in the child workspaces it splits there
/// and in its temporary directory.
pub(in crate::cli) async fn workspace_dir_args(
    paths: &crate::paths::Paths,
    settings: &Effective,
    name: &str,
    workspace: &Workspace,
) -> Vec<OsString> {
    let workspaces_dir = workspaces_dir(paths, workspace).await;
    dir_args(settings, name, &paths.home, workspaces_dir.as_deref())
}

/// The arguments that give agent `name` the configured agent directories as
/// other working directories, so it may read what the tools it runs leave
/// there without a permission prompt.
pub(crate) fn agent_dir_args(settings: &Effective, name: &str, home: &Path) -> Vec<OsString> {
    let arguments = &settings.launch(name).arguments;
    agent_dirs(settings, home)
        .iter()
        .flat_map(|directory| arguments.add_dir_args(directory))
        .collect()
}

/// The arguments that give agent `name` of `chat` the configured agent
/// directories, the chat's temporary directory and `directories` as other
/// working directories.
pub(in crate::cli) fn chat_dir_args(
    settings: &Effective,
    name: &str,
    home: &Path,
    chat: &Chat,
    directories: &[PathBuf],
) -> Vec<OsString> {
    let tmp_dir = chat
        .tmp_dir
        .as_ref()
        .map(|directory| std::fs::canonicalize(directory).unwrap_or_else(|_| directory.clone()));
    let mut args = dir_args(settings, name, home, tmp_dir.as_deref());
    let arguments = &settings.launch(name).arguments;
    args.extend(
        directories
            .iter()
            .flat_map(|directory| arguments.add_dir_args(directory)),
    );
    args
}

/// The arguments that give agent `name` the configured agent directories and
/// `extra`, resolved as agents compare paths, as other working directories.
fn dir_args(settings: &Effective, name: &str, home: &Path, extra: Option<&Path>) -> Vec<OsString> {
    let mut args = agent_dir_args(settings, name, home);
    if let Some(directory) = extra {
        args.extend(settings.launch(name).arguments.add_dir_args(directory));
    }
    args
}

/// The configured agent directories, `~/` naming a path under `home`,
/// resolved as agents compare paths, each once.
fn agent_dirs(settings: &Effective, home: &Path) -> BTreeSet<PathBuf> {
    settings
        .agent
        .dirs
        .iter()
        .map(|directory| fsutil::expand_home(Path::new(directory), home))
        .map(|directory| std::fs::canonicalize(&directory).unwrap_or(directory))
        .collect()
}

/// The directory in which `workspace`'s repository keeps its workspaces,
/// resolved as agents compare paths; `None`, with a warning when it cannot be
/// looked up, when there is none.
async fn workspaces_dir(paths: &crate::paths::Paths, workspace: &Workspace) -> Option<PathBuf> {
    let directory = client::repositories(paths).await.and_then(|repositories| {
        repositories
            .into_iter()
            .find(|repository| repository.id == workspace.repository_id)
            .map(|repository| repository.workspaces_dir)
            .context("workspace repository is missing")
    });
    match directory {
        Ok(directory) => {
            directory.map(|directory| std::fs::canonicalize(&directory).unwrap_or(directory))
        }
        Err(error) => {
            eprintln!("warning: could not give the agent the workspaces directory: {error:#}");
            None
        }
    }
}

/// Arguments that register Shoal's hooks for a chat of an agent with
/// integration `kind`.
pub(in crate::cli) fn chat_hook_args(kind: IntegrationKind) -> std::io::Result<Vec<OsString>> {
    integration(kind).chat_hook_args()
}

/// Start a chat of agent `name` its integration's own way; `None` leaves it
/// to the configured command run in place of Shoal.
pub(in crate::cli) async fn start_chat(
    ctx: &Context,
    name: &str,
    chat: &ChatLaunch,
) -> Result<Option<i32>> {
    let integration = agent_integration(&chat.settings, name);
    match integration.detached() {
        Some(app) => detached::start_chat(ctx, integration, app, name, chat)
            .await
            .map(Some),
        None => Ok(None),
    }
}

/// When the wrapper of an agent wakes it by typing a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptWake {
    /// Whenever it idles: no hook of its own starts its turn.
    Always,
    /// While it idles after an interrupted turn, after which the hook that
    /// wakes it otherwise does not run.
    AfterInterrupt,
}

/// The key that cancels the running turn of agent `name` under `settings`,
/// unless Shoal knows none.
pub(in crate::cli) fn interrupt_key(settings: &Effective, name: &str) -> Option<&'static str> {
    agent_integration(settings, name).interrupt_key()
}

/// When the wrapper of an agent with integration `kind` wakes it by typing
/// a prompt, unless it never does.
pub(crate) fn prompt_wake(kind: IntegrationKind) -> Option<PromptWake> {
    let integration = integration(kind);
    match integration.wakes() {
        Wake::Prompt => Some(PromptWake::Always),
        Wake::Hook if integration.interrupt_key().is_some() => Some(PromptWake::AfterInterrupt),
        Wake::Hook | Wake::Never => None,
    }
}

/// The failure an agent with integration `kind` reports in `event`.
pub(in crate::cli) fn turn_failure(
    kind: IntegrationKind,
    event: &serde_json::Value,
) -> Option<crate::state::TurnFailure> {
    integration(kind).turn_failure(event)
}

/// Trust the path of `workspace`, reopened from `removed`, for each agent
/// whose session `shoal resume` continues there as a tracked agent, since
/// resuming does not trust it as a launch does. Failures only warn.
pub(in crate::cli) async fn trust_resumed(
    ctx: &Context,
    workspace: &Workspace,
    removed: &RemovedWorkspace,
) {
    let stopped = removed
        .agents
        .iter()
        .filter(|agent| agent.record.session.is_some())
        .map(|agent| agent.record.agent.clone());
    let last = removed
        .last_sessions
        .iter()
        .filter(|last| last.session.is_some())
        .map(|last| last.agent.clone());
    trust_for(ctx, workspace, stopped.chain(last).collect()).await;
}

/// Trust the path of `workspace`, which moved, for each agent it saves for
/// `shoal resume`, which restores them there without trusting it.
pub(in crate::cli) async fn trust_saved(ctx: &Context, workspace: &Workspace) {
    match saved_agents(&ctx.paths, &workspace.id) {
        Ok(agents) => trust_for(ctx, workspace, agents).await,
        Err(error) => eprintln!(
            "warning: cannot trust {} for its agents: {error:#}",
            workspace.path.display()
        ),
    }
}

/// The agents the workspace saves for `shoal resume`.
fn saved_agents(paths: &Paths, workspace_id: &str) -> Result<Vec<String>> {
    let saved = recovery::Saved::load(paths, workspace_id)?;
    let mut agents = Vec::new();
    for (_, path) in &saved.agents {
        agents.extend(recovery::Record::load(path)?.map(|record| record.agent));
    }
    for (_, path) in &saved.sessions {
        agents.extend(recovery::LastSession::load(path)?.map(|last| last.agent));
    }
    Ok(agents)
}

/// Trust the path of `workspace` for each of `agents` as a tracked launch
/// does. Failures only warn.
async fn trust_for(ctx: &Context, workspace: &Workspace, agents: Vec<String>) {
    let settings =
        match client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!(
                    "warning: cannot trust {} for its agents: {error:#}",
                    workspace.path.display()
                );
                return;
            }
        };
    for name in &agents {
        if let Some(trust) = agent_integration(&settings, name).trust_file() {
            let profile = &settings.launch(name).environment;
            trust.trust_tracked(ctx, workspace, profile).await;
        }
    }
}

/// Trust a repository's checkout for a chat of agent `name` as workspace
/// launches trust theirs. Failure only warns.
pub(in crate::cli) async fn trust_checkout(
    ctx: &Context,
    settings: &Effective,
    name: &str,
    repository: &Repository,
) {
    if let Some(trust) = agent_integration(settings, name).trust_file() {
        trust
            .trust_checkout(ctx, repository, &settings.launch(name).environment)
            .await;
    }
}

/// Remove every integration's trust entries for a repository after its
/// deletion succeeds, as [`cleanup_moved_trust`] does, then have the daemon
/// forget where its agents kept their configs. Cleanup is best effort and
/// never fails the removal.
pub(crate) async fn cleanup_repository_trust(
    ctx: &Context,
    repository: &Repository,
    workspace_paths: &[PathBuf],
) {
    if cleanup_trust(ctx, repository, workspace_paths).await {
        forget_agent_environments(ctx, repository).await;
    }
}

/// Remove every integration's trust entries for the paths a repository had
/// before it moved, `repository` as recorded then, both where agents running
/// with this process's environment keep them and where the daemon kept that
/// its agents ran with. Failures only warn.
pub(crate) async fn cleanup_moved_trust(
    ctx: &Context,
    repository: &Repository,
    workspace_paths: &[PathBuf],
) {
    cleanup_trust(ctx, repository, workspace_paths).await;
}

/// Remove the trust entries [`cleanup_moved_trust`] removes, warning about
/// each failure; whether the daemon said where its agents kept their
/// configs.
async fn cleanup_trust(
    ctx: &Context,
    repository: &Repository,
    workspace_paths: &[PathBuf],
) -> bool {
    let agent_environments = agent_environments(ctx, repository).await;
    let warnings = forget_repository_trust(
        &ctx.paths.home,
        repository,
        workspace_paths,
        agent_environments.as_deref().unwrap_or_default(),
    );
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    agent_environments.is_some()
}

/// Where the removed repository's tracked agents kept their configuration;
/// `None` with a warning when the daemon cannot say.
async fn agent_environments(
    ctx: &Context,
    repository: &Repository,
) -> Option<Vec<BTreeMap<String, String>>> {
    client::pages(
        &ctx.paths,
        RepositoryMethod::AgentEnvironments {
            repository_id: repository.id.clone(),
        },
    )
    .await
    .inspect_err(|error| {
        eprintln!(
            "warning: could not clean trust entries where [env] put agent configs: {error:#}"
        );
    })
    .ok()
}

/// Have the daemon forget the removed repository's agent configs once their
/// trust entries are clean. Failure only warns.
async fn forget_agent_environments(ctx: &Context, repository: &Repository) {
    let forgotten = client::call(
        &ctx.paths,
        RepositoryMethod::ForgetAgentEnvironments {
            repository_id: repository.id.clone(),
        },
    )
    .await;
    if let Err(error) = forgotten {
        eprintln!("warning: the daemon keeps the removed repository's agent configs: {error:#}");
    }
}

/// Remove a deleted repository's trust entries from the configs of this
/// process's environment and of `agent_environments`, under `home`; each
/// failure comes back as a warning.
pub(crate) fn forget_repository_trust(
    home: &Path,
    repository: &Repository,
    workspace_paths: &[PathBuf],
    agent_environments: &[BTreeMap<String, String>],
) -> Vec<String> {
    let paths = trust::repository_paths(repository, workspace_paths);
    let configured = configured_environments(agent_environments);
    let environments: Vec<_> = std::iter::once(AgentEnvironment::inherited(home))
        .chain(
            configured
                .iter()
                .map(|configured| AgentEnvironment { home, configured }),
        )
        .collect();
    let mut cleaned: Vec<&trust::TrustFile> = Vec::new();
    let mut warnings = Vec::new();
    let integrations = IntegrationKind::ALL
        .iter()
        .map(|kind| integration(*kind))
        .chain(ADDED.iter().map(|added| added.integration));
    for integration in integrations {
        let Some(trust) = integration.trust_file() else {
            continue;
        };
        if !cleaned.iter().any(|done| std::ptr::eq(*done, trust)) {
            warnings.extend(trust.forget(&environments, &paths));
            cleaned.push(trust);
        }
    }
    warnings
}

/// `agent_environments` as the `[env]` values agents run with.
fn configured_environments(
    agent_environments: &[BTreeMap<String, String>],
) -> Vec<BTreeMap<String, OsString>> {
    agent_environments
        .iter()
        .map(|environment| {
            environment
                .iter()
                .map(|(name, value)| (name.clone(), value.into()))
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests;
