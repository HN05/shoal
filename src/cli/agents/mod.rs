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
pub(in crate::cli) mod hooks;
mod launch;
mod sessions;
mod trust;

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::{
    agent::{Agent, ModelChoice},
    ai::IntegrationKind,
    cli::{commands::configured, context::Context, terminal, ui},
    client::transcripts::Environments,
    config::{Config, Effective, modes, named_commands::Commands, templates},
    execution::recovery,
    fsutil,
    model::{Chat, ChatHome, LinkedItem, Relation, RemovedWorkspace, Repository, Workspace},
    paths::Paths,
    protocol::ConfigTarget,
};

pub(in crate::cli) use crate::client::transcripts::AgentEnvironment;
pub(in crate::cli) use desktop::{app, ensure_opens, hand_off};
pub(in crate::cli) use launch::input;
pub(in crate::cli) use sessions::{Existing, SessionHome, install_session_files};

/// What an agent needs beyond its configured command and described
/// arguments. Each default adds nothing.
pub(in crate::cli) trait Integration: Sync {
    /// Arguments that register Shoal's hooks for the session of `space`'s
    /// agent and, when `proxied`, what [`Self::route_through_proxy`] points
    /// at Shoal's proxy. They inherit the space's identity.
    fn hook_args(&self, _space: AgentSpace, _proxied: bool) -> std::io::Result<Vec<OsString>> {
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

/// An agent an integration adds besides the AI tools, with the integration
/// that runs it and why `settings` turn that integration off, if they do;
/// pickers and completion leave the agents of one that is off out.
struct Added {
    name: &'static str,
    integration: &'static dyn Integration,
    off: fn(&Effective) -> Option<&'static str>,
}

/// Agents integrations add, built only with each integration's Cargo
/// feature.
#[cfg(feature = "happy")]
const ADDED: &[Added] = &[
    Added {
        name: "happy-claude",
        integration: &happy::CLAUDE,
        off: happy::off,
    },
    Added {
        name: "happy-codex",
        integration: &happy::CODEX,
        off: happy::off,
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

/// How updates reach the chat agent `name` launched under `settings`.
pub(in crate::cli) fn chat_delivery(settings: &Effective, name: &str) -> templates::Delivery {
    launch::chat_delivery(agent_integration(settings, name))
}

/// Why `settings` turn agent `name` off, if they do: its own `enabled =
/// false`, or what turns off the integration that adds it.
fn agent_off(settings: &Effective, name: &str) -> Option<&'static str> {
    if settings.turned_off(name) {
        return Some("its [agents] entry sets enabled = false");
    }
    match (settings.integration(name), added(name)) {
        (IntegrationKind::None, Some(added)) => (added.off)(settings),
        _ => None,
    }
}

/// One launch of an agent in a ready workspace; settings are never cached
/// across launches.
pub(in crate::cli) struct Launch {
    pub name: String,
    pub workspace: Workspace,
    pub settings: Effective,
    /// Shoal's guidance and the mode's rules.
    pub instructions: String,
    /// The first message, such as an issue with the mode's instructions.
    pub prompt: Option<String>,
    pub args: Vec<OsString>,
    /// The arguments that start the agent from a fork of a session, which
    /// lead the others as a subcommand does.
    pub fork: Vec<OsString>,
    /// The model and effort it runs with.
    pub model: ModelChoice,
    /// The mode it starts in.
    pub mode: String,
    /// Recovery records of stopped agents this one replaces, consumed once
    /// it has started.
    pub replaces: Vec<PathBuf>,
}

/// The repository a chat belongs to, or none, with `root_dir`, where a chat
/// without one runs.
#[derive(Debug, Clone)]
pub(in crate::cli) enum ChatRepository {
    Registered(Repository),
    Outside(PathBuf),
}

impl ChatRepository {
    /// The registered repository; none for a chat without one.
    pub fn registered(&self) -> Option<&Repository> {
        match self {
            Self::Registered(repository) => Some(repository),
            Self::Outside(_) => None,
        }
    }

    /// Where a chat runs: the worktree of `workspace` when given, else the
    /// repository's checkout, or `root_dir`.
    pub fn home<'a>(&'a self, workspace: Option<&'a Workspace>) -> ChatHome<'a> {
        match (workspace, self) {
            (Some(workspace), _) => ChatHome::Workspace(workspace),
            (None, Self::Registered(repository)) => ChatHome::Checkout(repository),
            (None, Self::Outside(root)) => ChatHome::Root(root),
        }
    }

    /// The config a chat's settings come from.
    pub fn config(&self) -> ConfigTarget {
        match self {
            Self::Registered(repository) => ConfigTarget::Repository(repository.id.clone().into()),
            Self::Outside(_) => ConfigTarget::Global,
        }
    }
}

/// One chat about a repository in its registered checkout, or in a
/// workspace's worktree, or without a repository in `root_dir`.
pub(in crate::cli) struct ChatLaunch {
    pub repository: ChatRepository,
    /// The workspace whose worktree the chat runs in, instead of the
    /// repository's checkout.
    pub workspace: Option<Workspace>,
    /// The name the chat is recorded under.
    pub name: String,
    /// The mode its agent starts in.
    pub mode: String,
    /// The space it goes below.
    pub parent: Option<String>,
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
    /// The stopped chat, by ID, it restores instead of recording a new one.
    pub restores: Option<String>,
    /// What the chat is linked to once recorded, such as the issue it plans.
    pub links: Vec<(Relation, LinkedItem)>,
}

impl ChatLaunch {
    /// Where the chat runs.
    pub fn home(&self) -> ChatHome<'_> {
        self.repository.home(self.workspace.as_ref())
    }
}

/// Assign the recorded `chat` the issue its launch names, if any. A failure
/// is told and leaves the chat to start without it.
pub(in crate::cli) async fn link_chat(
    ctx: &Context,
    chat: &Chat,
    links: &[(Relation, LinkedItem)],
) {
    for (relation, item) in links {
        if let Err(error) = crate::client::items::link(&ctx.paths, &chat.id, *relation, item).await
        {
            eprintln!("shoal: {} was not linked to the chat: {error:#}", item.url);
        }
    }
}

/// What a workspace agent's launch is told: its mode decides the rules
/// of its system text and the instructions after its subject.
pub(in crate::cli) enum Opening {
    /// A new session, starting on `subject` when it has one.
    New {
        mode: String,
        subject: Option<templates::Subject>,
    },
    /// A session in `mode` told `message` as it is, such as one that
    /// replaces a stopped session.
    Told { mode: String, message: String },
}

impl Opening {
    fn mode(&self) -> &str {
        match self {
            Self::New { mode, .. } | Self::Told { mode, .. } => mode,
        }
    }

    /// A new session of the default mode, on `subject` when given.
    pub fn worker(subject: Option<templates::Subject>) -> Self {
        Self::New {
            mode: modes::DEFAULT.to_owned(),
            subject,
        }
    }

    /// The system text and first message of the launch at `place`.
    fn brief(&self, settings: &Effective, place: templates::Place<'_>) -> Result<templates::Brief> {
        match self {
            Self::New { mode, subject } => {
                templates::brief(settings, mode, place, subject.as_ref())
            }
            Self::Told { mode, message } => Ok(templates::Brief {
                system: templates::system(settings, mode, place, linked_issue(place).as_ref())?,
                first_message: Some(message.clone()),
            }),
        }
    }
}

/// The issue the workspace at `place` is linked to, for the rules of an
/// agent that starts without its subject.
fn linked_issue(place: templates::Place<'_>) -> Option<templates::IssueFields> {
    match place {
        templates::Place::Workspace { workspace, .. } => {
            templates::IssueFields::linked(&workspace.links)
        }
        templates::Place::Chat { .. } => None,
    }
}

/// How a launch relates to the agents around it.
#[derive(Default)]
pub(in crate::cli) struct LaunchOptions {
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
pub(in crate::cli) fn runs_in_session(agent: &Agent, settings: &Effective) -> bool {
    !runs_its_own_way(agent, settings) && terminal::keeps_agents_in_sessions(settings)
}

/// Whether a launch of `agent` under `settings` leaves the launching process
/// at once, as one its integration runs detached, such as Happy, does.
pub(in crate::cli) fn runs_its_own_way(agent: &Agent, settings: &Effective) -> bool {
    agent_integration(settings, agent.name())
        .detached()
        .is_some()
}

/// Start an agent in a ready workspace, giving it the prompt the way it
/// accepts one.
pub(super) async fn launch_agent(
    ctx: &Context,
    agent: Agent,
    workspace: Workspace,
    opening: Opening,
    args: Vec<OsString>,
    options: LaunchOptions,
) -> Result<i32> {
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(workspace.id.clone()),
    )
    .await?;
    let integration = agent_integration(&settings, agent.name());
    let fork = match &options.fork {
        Some(session) => fork_args(&settings, agent.name(), session, &workspace.path)?,
        None => Vec::new(),
    };
    let place = templates::Place::Workspace {
        workspace: &workspace,
        delivery: launch::delivery(integration, &settings),
    };
    let templates::Brief {
        system: instructions,
        first_message: prompt,
    } = opening.brief(&settings, place)?;
    let model = match options.model {
        ModelRequest::Override(requested) => resolve_model(&settings, agent.name(), requested),
        ModelRequest::Kept(model) => model,
    };
    let launch = Launch {
        name: agent.into(),
        workspace,
        settings,
        instructions,
        prompt,
        args,
        fork,
        model,
        mode: opening.mode().to_owned(),
        replaces: options.replaces,
    };
    let replaces = launch.replaces.clone();
    let Some(detached) = integration.detached() else {
        return launch::tracked(ctx, integration, launch).await;
    };
    let code = detached::start(ctx, integration, detached, launch).await?;
    // A launch of its own returns once its agent has started.
    for record in &replaces {
        crate::execution::recovery::consume(record)?;
    }
    Ok(code)
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
        Some(target) => crate::client::workspaces::settings(&ctx.paths, target)
            .await
            .ok(),
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
    let Ok(workspaces) = crate::client::workspaces::list(&ctx.paths).await else {
        return Vec::new();
    };
    let mut defining = Vec::new();
    for workspace in workspaces {
        let target = ConfigTarget::Workspace(workspace.id.clone());
        // A workspace whose configuration cannot load has no agents.
        if let Ok(settings) = crate::client::workspaces::settings(&ctx.paths, target).await
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
    let Ok(workspaces) = crate::client::workspaces::list(&ctx.paths).await else {
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
    let settings = crate::client::workspaces::settings(&ctx.paths, target).await?;
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
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    pick_agent(ctx, settings, |name| {
        installed(name, &settings.launchers, &search_path)
    })
}

/// The picker standing in for a missing reviewer: the agents with a review
/// template whose review command is installed.
pub(in crate::cli) fn pick_reviewer(ctx: &Context, settings: &Effective) -> Result<Option<Agent>> {
    pick_agent(ctx, settings, |name| {
        settings.launch(name).arguments.review.is_some()
            && ensure_reviewer_available(name, settings).is_ok()
    })
}

/// A picker of the agents `available` accepts, with “No agent”.
fn pick_agent(
    ctx: &Context,
    settings: &Effective,
    available: impl Fn(&str) -> bool,
) -> Result<Option<Agent>> {
    ensure!(
        ctx.interactive(),
        "no agent selected; pass --agent or set agent.default in the repository or global config"
    );
    let mut choices: Vec<_> = names(settings)
        .into_iter()
        .filter(|name| available(name))
        .map(|name| (name.parse().ok(), name))
        .collect();
    // A picker cannot run without fzf. Treat that as “No agent” so callers
    // without the optional interactive dependency retain the existing flow.
    if choices.is_empty() || !ui::can_pick(ctx) {
        return Ok(None);
    }
    choices.push((None, "No agent".into()));
    let choices: Vec<_> = choices
        .iter()
        .map(|(agent, label)| (agent.clone(), label.as_str()))
        .collect();
    let picked: Option<Agent> = ui::pick_choice(ctx, "Agent> ", &choices)?;
    // The picker offers agents by their own programs; the launch may need
    // a session host too, refused before anything is created.
    if let Some(agent) = &picked {
        ensure_available(agent.name(), settings)?;
    }
    Ok(picked)
}

/// The agents pickers and completion offer under `settings`, built-in
/// providers first, leaving out those `settings` turn off.
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
    for Added { name, .. } in ADDED {
        if !names.iter().any(|known| known == name) {
            names.push((*name).to_owned());
        }
    }
    names.retain(|name| agent_off(settings, name).is_none());
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

/// Whether every executable the agent itself starts is available, so pickers
/// and completion offer only agents that can launch; a launch refuses a
/// missing session host itself.
pub(in crate::cli) fn installed(name: &str, commands: &Commands, search_path: &OsStr) -> bool {
    programs(name, commands, None).is_some_and(|programs| {
        programs
            .into_iter()
            .all(|program| program_available(program, search_path))
    })
}

/// Refuse a reviewer without a review template or whose review command's
/// program is missing. A review runs without a session, so it needs no
/// terminal host.
pub(in crate::cli) fn ensure_reviewer_available(name: &str, settings: &Effective) -> Result<()> {
    settings.check_reviewer(name)?;
    let program = settings
        .launchers
        .get(name)
        .and_then(|argv| argv.first())
        .with_context(|| {
            format!("unknown agent {name:?}; set [agents.{name}] command in Shoal config")
        })?;
    ensure_program(name, program)
}

/// Refuse an agent that cannot run under `settings` before anything starts.
pub(crate) fn ensure_available(name: &str, settings: &Effective) -> Result<()> {
    if let Some(reason) = agent_off(settings, name) {
        anyhow::bail!("cannot start {name}: {reason}");
    }
    launch_proxy(settings, name)?;
    ensure_installed(name, settings)
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
pub(in crate::cli) fn fork_args(
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

/// Refuse an agent whose executables are missing, the session host a launch
/// here under `settings` starts it in among them.
fn ensure_installed(name: &str, settings: &Effective) -> Result<()> {
    let launcher = super::terminal::launcher_program(settings);
    let programs = programs(name, &settings.launchers, launcher).with_context(|| {
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

/// The executables an agent starts; `None` when it has no launcher. A
/// terminal agent also starts `launcher`, its session host's program.
fn programs<'a>(
    name: &str,
    commands: &'a Commands,
    launcher: Option<&'static str>,
) -> Option<Vec<&'a str>> {
    if let Some(programs) = added(name).and_then(|added| added.integration.programs()) {
        return Some(programs);
    }
    commands
        .get(name)
        .and_then(|argv| argv.first())
        .map(|program| {
            let mut programs = vec![program.as_str()];
            programs.extend(launcher);
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

/// What a workspace's agent learns from its hooks about the workspace it
/// works in.
#[derive(Clone, Copy)]
pub(crate) struct AgentWorkspace<'a> {
    pub name: &'a str,
    /// `None` for a workspace at a commit.
    pub branch: Option<&'a str>,
}

impl<'a> From<&'a Workspace> for AgentWorkspace<'a> {
    fn from(workspace: &'a Workspace) -> Self {
        Self {
            name: &workspace.name,
            branch: workspace.branch.as_deref(),
        }
    }
}

/// The space whose agent Shoal's hooks serve: a workspace, whose agent
/// also gets the hooks and rules that need its worktree, or a chat.
#[derive(Clone, Copy)]
pub(crate) enum AgentSpace<'a> {
    Workspace(AgentWorkspace<'a>),
    Chat,
}

impl<'a> AgentSpace<'a> {
    /// The workspace, unless the space is a chat.
    pub fn workspace(self) -> Option<AgentWorkspace<'a>> {
        match self {
            Self::Workspace(workspace) => Some(workspace),
            Self::Chat => None,
        }
    }
}

impl<'a> From<AgentWorkspace<'a>> for AgentSpace<'a> {
    fn from(workspace: AgentWorkspace<'a>) -> Self {
        Self::Workspace(workspace)
    }
}

impl<'a> From<&'a Workspace> for AgentSpace<'a> {
    fn from(workspace: &'a Workspace) -> Self {
        Self::Workspace(workspace.into())
    }
}

/// Arguments that register Shoal's hooks for the session of `space`'s agent
/// with integration `kind`, and when `proxied` route its API requests
/// through Shoal's proxy.
pub(crate) fn hook_args(
    kind: IntegrationKind,
    space: AgentSpace,
    proxied: bool,
) -> std::io::Result<Vec<OsString>> {
    integration(kind).hook_args(space, proxied)
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
    let environment = AgentEnvironment::new(home, configured);
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

/// The arguments that give agent `name`, restored in `workspace`, the
/// system text of `mode` again, which a restored session started without;
/// none for an agent that takes it only in its first message, which its
/// session already has. A mode no config defines anymore gives the default
/// mode's, with a warning, so a restore never waits on config.
pub(crate) fn restored_instruction_args(
    settings: &Effective,
    name: &str,
    workspace: &Workspace,
    mode: &str,
) -> Vec<OsString> {
    let place = templates::Place::Workspace {
        workspace,
        delivery: launch::delivery(agent_integration(settings, name), settings),
    };
    let issue = templates::IssueFields::linked(&workspace.links);
    let system = templates::system(settings, mode, place, issue.as_ref()).or_else(|error| {
        eprintln!(
            "warning: the restored agent is told the rules of mode {} instead: {error:#}",
            modes::DEFAULT
        );
        templates::system(settings, modes::DEFAULT, place, issue.as_ref())
    });
    match system {
        Ok(system) => (settings.launch(name).arguments)
            .instruction_args(&system)
            .unwrap_or_default(),
        Err(error) => {
            eprintln!("warning: the restored agent is not told its rules: {error:#}");
            Vec::new()
        }
    }
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
    let mut args = hook_args(settings.integration(name), workspace.into(), proxied)?;
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
    let directory = crate::client::repositories::list(paths)
        .await
        .and_then(|repositories| {
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
    let settings = match crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(workspace.id.clone()),
    )
    .await
    {
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

/// Trust the directory of a chat of agent `name` in `repository`, or
/// without one, at `home` as workspace launches trust theirs. Failure only
/// warns.
pub(in crate::cli) async fn trust_chat(
    ctx: &Context,
    settings: &Effective,
    name: &str,
    repository: Option<&Repository>,
    home: ChatHome<'_>,
) {
    if let Some(trust) = agent_integration(settings, name).trust_file() {
        let profile = &settings.launch(name).environment;
        trust.trust_chat(ctx, repository, home, profile).await;
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
    crate::client::repositories::agent_environments(&ctx.paths, repository.id.clone())
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
    let forgotten =
        crate::client::repositories::forget_agent_environments(&ctx.paths, repository.id.clone())
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
    let environments = Environments::new(home, agent_environments);
    let environments = environments.each();
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

#[cfg(test)]
mod tests;
