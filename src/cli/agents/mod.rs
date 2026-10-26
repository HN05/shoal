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
mod shortcuts;
mod trust;

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::{
    agent::Agent,
    ai::IntegrationKind,
    cli::{client, commands::configured, context::Context, terminal, ui},
    config::{Config, Effective, named_commands::Commands},
    fsutil,
    model::{RemovedWorkspace, Repository, Workspace},
    protocol::{ConfigTarget, RepositoryMethod},
    tools::Tool,
};

pub(in crate::cli) use launch::input;
#[cfg(feature = "happy")]
pub(super) use shortcuts::happy;
pub(in crate::cli) use shortcuts::{Installed, happy_agents, shortcuts};
pub(super) use shortcuts::{codex, open_app};

/// What an agent needs beyond its configured command and described
/// arguments. Each default adds nothing.
pub(in crate::cli) trait Integration: Sync {
    /// Arguments that register Shoal's hooks for the agent's session and,
    /// when `proxied`, send its API requests to Shoal's proxy, with
    /// placeholders that [`Self::route_through_proxy`] fills in.
    fn hook_args(&self, _proxied: bool) -> std::io::Result<Vec<OsString>> {
        Ok(Vec::new())
    }

    /// Whether its launches can send their API requests to Shoal's proxy,
    /// which [`Self::hook_args`] and [`Self::route_through_proxy`] then do.
    fn routes_through_proxy(&self) -> bool {
        false
    }

    /// Fill in the proxy placeholders of `args` with the proxy's `url`, for
    /// the agent running with `environment`.
    fn route_through_proxy(
        &self,
        args: &mut Vec<OsString>,
        url: &str,
        _environment: &AgentEnvironment,
    ) -> Result<()> {
        crate::proxy::fill_url(args, url);
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

    /// The programs a launch starts, when not its configured command's.
    fn programs(&self) -> Option<Vec<&'static str>> {
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
    /// The program of the desktop app config hands the agent's launches to.
    pub app: Option<&'static str>,
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

/// What `settings` set for the integration of AI tools of `kind`, from its
/// own section.
fn tool_options(kind: IntegrationKind, settings: &Effective) -> Options {
    match kind {
        IntegrationKind::Codex => codex::options(&settings.codex),
        IntegrationKind::Claude | IntegrationKind::None => Options::default(),
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
        options: |settings| happy::options(&settings.happy),
    },
    Added {
        name: "happy-codex",
        integration: &happy::CODEX,
        options: |settings| happy::options(&settings.happy),
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
        (kind, _) => tool_options(kind, settings),
    }
}

/// The desktop app a launch with `options` opens: config hands launches to
/// it, the app cannot take a first message, so a `prompted` launch stays
/// in the terminal, and a `tracked` launch is never handed to an app.
fn opens_app(options: Options, prompted: bool, tracked: bool) -> Option<&'static str> {
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
    let app = opens_app(
        agent_options(&settings, agent.name()),
        prompt.is_some(),
        options.tracked,
    );
    let launch = Launch {
        name: agent.into(),
        workspace,
        settings,
        prompt,
        args,
        replaces: options.replaces,
    };
    let replaces = launch.replaces.clone();
    let code = match (app, integration.detached()) {
        (Some(app), _) => {
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

/// Whether `shoal <name>` launches an agent rather than a configured command.
pub(super) fn is_agent(ctx: &Context, name: &str) -> Result<bool> {
    let tools = Config::load(&ctx.paths)?.ai.tools;
    Ok(crate::ai::agents(&tools).any(|agent| agent == name) || added(name).is_some())
}

/// `shoal <name>` and `shoal run <name>`: agents launch with their prompts,
/// other names run as plain configured commands.
pub(super) async fn run_named(
    ctx: &Context,
    name: &str,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    if !is_agent(ctx, name)? {
        return configured::run(ctx, name, workspace, args).await;
    }
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

/// The requested agent, else the configured `default_agent`, else a picker.
pub(super) async fn default_agent(
    ctx: &Context,
    target: ConfigTarget,
    agent: Option<Agent>,
) -> Result<Option<Agent>> {
    let settings = client::settings(&ctx.paths, target).await?;
    match agent.or(settings.default_agent.clone()) {
        Some(agent) => {
            ensure_available(agent.name(), &settings)?;
            Ok(Some(agent))
        }
        None => pick_default_agent(ctx, &settings),
    }
}

/// The picker standing in for a missing `default_agent`.
pub(super) fn pick_default_agent(ctx: &Context, settings: &Effective) -> Result<Option<Agent>> {
    ensure!(
        ctx.interactive(),
        "no agent selected; pass --agent or set default_agent in the repository or global config"
    );
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let mut choices: Vec<_> = names(settings)
        .into_iter()
        .filter(|name| installed(name, &settings.commands, &search_path))
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
pub(in crate::cli) fn ensure_available(name: &str, settings: &Effective) -> Result<()> {
    if let Some(reason) = agent_options(settings, name).off {
        anyhow::bail!("cannot start {name}: {reason}");
    }
    ensure!(
        settings.proxy(name).is_none()
            || integration(settings.integration(name)).routes_through_proxy(),
        "cannot start {name}: Shoal cannot send its API requests through a proxy; \
         remove proxy from its [ai.{name}]"
    );
    ensure_installed(name, &settings.commands)
}

/// Refuse an agent whose executables are missing.
fn ensure_installed(name: &str, commands: &Commands) -> Result<()> {
    let programs = programs(name, commands).with_context(|| {
        format!("unknown agent {name:?}; set [ai.{name}] command in Shoal config")
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

/// Whether an agent with integration `kind` can go through Shoal's proxy.
pub(crate) fn routes_through_proxy(kind: IntegrationKind) -> bool {
    integration(kind).routes_through_proxy()
}

/// `command` of an agent with integration `kind`, running with `configured`
/// added to this process's environment, sent to the proxy at `url`.
pub(crate) fn route_through_proxy(
    kind: IntegrationKind,
    mut command: Vec<OsString>,
    url: &str,
    home: &Path,
    configured: &BTreeMap<String, OsString>,
) -> Result<Vec<OsString>> {
    let environment = AgentEnvironment { home, configured };
    integration(kind).route_through_proxy(&mut command, url, &environment)?;
    Ok(command)
}

/// What a workspace's agent `name` runs with besides its instructions, prompt
/// and forwarded arguments: Shoal's hooks, and the repository's workspaces
/// directory as another working directory, so it may work in the child
/// workspaces it splits there.
pub(crate) async fn workspace_args(
    paths: &crate::paths::Paths,
    settings: &Effective,
    name: &str,
    workspace: &Workspace,
) -> Result<Vec<OsString>> {
    let proxied = settings.proxy(name).is_some();
    let mut args = hook_args(settings.integration(name), proxied)?;
    if let Some(directory) = workspaces_dir(paths, workspace).await {
        args.extend(settings.launch(name).arguments.add_dir_args(&directory));
    }
    Ok(args)
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

/// Whether the wrapper of an agent with integration `kind` wakes it by
/// typing a prompt.
pub(crate) fn wakes_by_prompt(kind: IntegrationKind) -> bool {
    integration(kind).wakes() == Wake::Prompt
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
    let settings =
        match client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!("warning: cannot trust the restored workspace for its agents: {error:#}");
                return;
            }
        };
    let stopped = removed
        .agents
        .iter()
        .filter(|agent| agent.record.session.is_some())
        .map(|agent| agent.record.agent.as_str());
    let last = removed
        .last_session
        .iter()
        .filter(|last| last.session.is_some())
        .map(|last| last.agent.as_str());
    for name in stopped.chain(last) {
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
/// deletion succeeds, both where agents running with this process's
/// environment keep them and where the daemon kept that its agents ran
/// with, which it then forgets. Cleanup is best effort and never fails the
/// removal.
pub(crate) async fn cleanup_repository_trust(
    ctx: &Context,
    repository: &Repository,
    workspace_paths: &[PathBuf],
) {
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
    if agent_environments.is_some() {
        forget_agent_environments(ctx, repository).await;
    }
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
