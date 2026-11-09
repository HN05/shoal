//! The CLI's side of the agents Shoal launches (see [`crate::agents`]):
//! picking one, `shoal <name>`, launching one from a command in a
//! workspace or a chat, handing a workspace to a desktop app, and the trust
//! and session files those launches prepare.
mod desktop;
mod detached;
mod launch;
mod sessions;
mod trust;

use crate::client::space_ids::SpaceId;
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    agent::{Agent, ModelChoice},
    agents::{
        ADDED, Added, added, agent_integration, agent_off, dir_args, ensure_available,
        ensure_forks, integration, programs, resolve_model,
    },
    ai::IntegrationKind,
    cli::{commands::configured, context::Context, ui},
    config::{Config, Effective, modes, named_commands::Commands, templates},
    execution::recovery,
    model::{Chat, ChatHome, LinkedItem, Relation, RemovedWorkspace, Repository, Workspace},
    paths::Paths,
    protocol::ConfigTarget,
    tools::{ensure_program, program_available},
};

pub(in crate::cli) use desktop::{app, ensure_opens, hand_off};
pub(in crate::cli) use launch::input;
pub(in crate::cli) use sessions::{
    Existing, SessionHome, install_kept_session_files, install_session_files,
};
pub(in crate::cli) use trust::{cleanup_moved_trust, cleanup_repository_trust};

/// Where a chat of agent `name` runs when an integration it adds starts it
/// outside any terminal, such as an app.
pub(in crate::cli) fn chat_place(name: &Agent) -> Option<&'static str> {
    let app = added(name).and_then(|added| added.integration.detached());
    app.map(crate::agents::detached::App::place)
}

/// How updates reach the chat agent `name` launched under `settings`.
pub(in crate::cli) fn chat_delivery(settings: &Effective, name: &Agent) -> templates::Delivery {
    launch::chat_delivery(agent_integration(settings, name))
}

/// One launch of an agent in a ready workspace; settings are never cached
/// across launches.
pub(in crate::cli) struct Launch {
    pub name: Agent,
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
    pub mode: modes::ModeName,
    /// The permissions `--permission` gives the agent, whatever the mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
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
    pub name: crate::client::space_names::SpaceName,
    /// The mode its agent starts in.
    pub mode: modes::ModeName,
    /// The permissions `--permission` gives the agent, whatever the mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// The space it goes below.
    pub parent: Option<SpaceSelector>,
    pub settings: Effective,
    /// The rendered chat template.
    pub instructions: String,
    /// The task the chat starts on before the user speaks, if any.
    pub opening: Option<String>,
    /// The model and effort it runs with.
    pub model: ModelChoice,
    pub args: Vec<OsString>,
    /// The stopped chat, by ID, it restores instead of recording a new one.
    pub restores: Option<SpaceId>,
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
        if let Err(error) =
            crate::client::items::link(&ctx.paths, &SpaceSelector::id(&chat.id), *relation, item)
                .await
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
        mode: modes::ModeName,
        subject: Option<templates::Subject>,
    },
    /// A session in `mode` told `message` as it is, such as one that
    /// replaces a stopped session.
    Told {
        mode: modes::ModeName,
        message: String,
    },
}

impl Opening {
    fn mode(&self) -> &modes::ModeName {
        match self {
            Self::New { mode, .. } | Self::Told { mode, .. } => mode,
        }
    }

    /// A new session of the default mode, on `subject` when given.
    pub fn worker(subject: Option<templates::Subject>) -> Self {
        Self::New {
            mode: modes::ModeName::DEFAULT,
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
    pub fork: Option<crate::client::agent::AgentSessionId>,
    /// The model and effort the launch asks for.
    pub model: ModelRequest,
    /// The permissions `--permission` gives the agent, whatever the mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
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
    !runs_its_own_way(agent, settings) && crate::terminal::keeps_agents_in_sessions(settings)
}

/// Whether a launch of `agent` under `settings` leaves the launching process
/// at once, as one its integration runs detached, such as Happy, does.
pub(in crate::cli) fn runs_its_own_way(agent: &Agent, settings: &Effective) -> bool {
    agent_integration(settings, agent).detached().is_some()
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
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let integration = agent_integration(&settings, &agent);
    let fork = match &options.fork {
        Some(session) => fork_args(&settings, &agent, session, &workspace.path)?,
        None => Vec::new(),
    };
    let place = templates::Place::Workspace {
        workspace: &workspace,
        delivery: crate::agents::delivery(integration, &settings),
    };
    let templates::Brief {
        system: instructions,
        first_message: prompt,
    } = opening.brief(&settings, place)?;
    let model = match options.model {
        ModelRequest::Override(requested) => resolve_model(&settings, &agent, requested),
        ModelRequest::Kept(model) => model,
    };
    let launch = Launch {
        name: agent,
        workspace,
        settings,
        instructions,
        prompt,
        args,
        fork,
        model,
        mode: opening.mode().clone(),
        narrowing: options.narrowing,
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
pub(super) async fn is_agent(
    ctx: &Context,
    name: &str,
    workspace: Option<&SpaceSelector>,
) -> Result<bool> {
    Ok(!matches!(
        classify(ctx, name, workspace).await?,
        NamedRun::Command
    ))
}

/// Agents are those the targeted workspace's settings resolve, else this
/// machine's config; outside every workspace, also a repository's own,
/// which runs in a workspace defining it, as a repository's command does.
async fn classify(
    ctx: &Context,
    name: &str,
    workspace: Option<&SpaceSelector>,
) -> Result<NamedRun> {
    if (name.parse::<Agent>()).is_ok_and(|agent| added(&agent).is_some()) {
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
        let target = ConfigTarget::Workspace(SpaceSelector::id(&workspace.id));
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
async fn agent_target(
    ctx: &Context,
    workspace: Option<&SpaceSelector>,
) -> Result<Option<ConfigTarget>> {
    if let Some(workspace) = workspace {
        // A selector that does not resolve is the command's own error.
        let workspace = crate::cli::ui::explicit_workspace(ctx, workspace.clone()).await;
        return Ok(workspace
            .ok()
            .map(|selected| ConfigTarget::Workspace(selected.clone())));
    }
    // Without a daemon there is no workspace context, only the machine's.
    let Ok(workspaces) = crate::client::workspaces::list(&ctx.paths).await else {
        return Ok(None);
    };
    let current = configured::current_workspace(&workspaces)?;
    Ok(current.map(|workspace| ConfigTarget::Workspace(SpaceSelector::id(&workspace.id))))
}

/// `shoal <name>` and `shoal run <name>`: agents launch with their prompts,
/// other names run as plain configured commands.
pub(super) async fn run_named(
    ctx: &Context,
    name: &str,
    workspace: Option<SpaceSelector>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = match classify(ctx, name, workspace.as_ref()).await? {
        NamedRun::Command => return configured::run(ctx, name, workspace, args).await,
        NamedRun::Agent => workspace,
        NamedRun::Elsewhere(defining) => {
            ensure!(
                ctx.interactive(),
                "{name} is an agent only some repositories define; run it in one of their \
                 workspaces or name one with --workspace"
            );
            Some(SpaceSelector::id(
                &ui::pick_workspace(ctx, defining, "workspace").await?,
            ))
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
            ensure_available(&agent, &settings)?;
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
    available: impl Fn(&Agent) -> bool,
) -> Result<Option<Agent>> {
    ensure!(
        ctx.interactive(),
        "no agent selected; pass --agent or set agent.default in the repository or global config"
    );
    let agents = names(settings);
    let mut choices: Vec<_> = agents
        .iter()
        .filter(|agent| available(agent))
        .map(|agent| (Some(agent.clone()), agent.name()))
        .collect();
    // A picker cannot run without fzf. Treat that as “No agent” so callers
    // without the optional interactive dependency retain the existing flow.
    if choices.is_empty() || !ui::can_pick(ctx) {
        return Ok(None);
    }
    choices.push((None, "No agent"));
    let picked: Option<Agent> = ui::pick_choice(ctx, "Agent> ", &choices)?;
    // The picker offers agents by their own programs; the launch may need
    // a session host too, refused before anything is created.
    if let Some(agent) = &picked {
        ensure_available(agent, settings)?;
    }
    Ok(picked)
}

/// The agents pickers and completion offer under `settings`, built-in
/// providers first, leaving out those `settings` turn off.
pub(in crate::cli) fn names(settings: &Effective) -> Vec<Agent> {
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
    let mut agents: Vec<Agent> = names.iter().filter_map(|name| name.parse().ok()).collect();
    agents.retain(|agent| agent_off(settings, agent).is_none());
    agents
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
pub(in crate::cli) fn installed(name: &Agent, commands: &Commands, search_path: &OsStr) -> bool {
    programs(name, commands, None).is_some_and(|programs| {
        programs
            .into_iter()
            .all(|program| program_available(program, search_path))
    })
}

/// Refuse a reviewer without a review template or whose review command's
/// program is missing. A review runs without a session, so it needs no
/// terminal host.
pub(in crate::cli) fn ensure_reviewer_available(name: &Agent, settings: &Effective) -> Result<()> {
    settings.check_reviewer(name)?;
    let program = settings
        .launchers
        .get(name.name())
        .and_then(|argv| argv.first())
        .with_context(|| {
            format!("unknown agent {name:?}; set [agents.{name}] command in Shoal config")
        })?;
    ensure_program(name.name(), program)
}

/// The arguments that start agent `name` in `directory` from a fork of
/// `session`, refusing an agent [`ensure_forks`] refuses.
pub(in crate::cli) fn fork_args(
    settings: &Effective,
    name: &Agent,
    session: &crate::client::agent::AgentSessionId,
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

/// The arguments that give agent `name` of `chat` the configured agent
/// directories and the chat's temporary directory as other working
/// directories; for a setup chat without a repository also the directory
/// of the global config beside `home`, which it may write.
pub(in crate::cli) fn chat_dir_args(
    settings: &Effective,
    name: &Agent,
    home: &Path,
    chat: &Chat,
) -> Vec<OsString> {
    let tmp_dir = chat
        .tmp_dir
        .as_ref()
        .map(|directory| std::fs::canonicalize(directory).unwrap_or_else(|_| directory.clone()));
    let extra: Vec<_> = tmp_dir
        .into_iter()
        .chain(global_config_dir(chat, home))
        .collect();
    dir_args(settings, name, home, &extra)
}

/// The directory of the global config beside `home` when `chat` sets it up,
/// as a setup chat without a repository does.
fn global_config_dir(chat: &Chat, home: &Path) -> Option<PathBuf> {
    let sets_up_global = chat.mode == modes::ModeName::SETUP && chat.repository_id.is_none();
    sets_up_global
        .then(|| launch_config_dir(&Config::path_for_home(home)))
        .flatten()
}

/// The directory of the config file `file`, created when missing and
/// resolved as agents compare paths, so a setup agent may write there.
pub(in crate::cli) fn writable_config_dir(file: &Path) -> Result<PathBuf> {
    crate::config_file::refuse_user_config(file)?;
    let directory = file.parent().context("the config file has no directory")?;
    std::fs::create_dir_all(directory)
        .and_then(|()| std::fs::canonicalize(directory))
        .with_context(|| format!("create {}", directory.display()))
}

/// [`writable_config_dir`] for a launch, which starts without it, with a
/// warning, when it cannot be created.
fn launch_config_dir(file: &Path) -> Option<PathBuf> {
    writable_config_dir(file)
        .map_err(|error| eprintln!("warning: the agent may not write its config: {error:#}"))
        .ok()
}

/// Start a chat of agent `name` its integration's own way; `None` leaves it
/// to the configured command run in place of Shoal.
pub(in crate::cli) async fn start_chat(
    ctx: &Context,
    name: &Agent,
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

/// The key that cancels the running turn of agent `name` under `settings`,
/// unless Shoal knows none.
pub(in crate::cli) fn interrupt_key(settings: &Effective, name: &Agent) -> Option<&'static str> {
    agent_integration(settings, name).interrupt_key()
}

/// The failure an agent with integration `kind` reports in `event`.
pub(in crate::cli) fn turn_failure(
    kind: IntegrationKind,
    event: &serde_json::Value,
) -> Option<crate::state::TurnFailure> {
    integration(kind).turn_failure(event)
}

/// The plan change that `event`, from the plan hook of an agent with
/// integration `kind`, reports.
pub(in crate::cli) fn plan_change(
    kind: IntegrationKind,
    event: &serde_json::Value,
) -> Option<crate::client::progress::ProgressChange> {
    integration(kind).plan_change(event)
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
fn saved_agents(paths: &Paths, workspace_id: &SpaceId) -> Result<Vec<Agent>> {
    let saved = recovery::Saved::load(paths, workspace_id)?;
    let mut agents = Vec::new();
    for agent in saved.agent_names() {
        agents.extend(agent?);
    }
    Ok(agents)
}

/// Trust the path of `workspace` for each of `agents` as a tracked launch
/// does. Failures only warn.
async fn trust_for(ctx: &Context, workspace: &Workspace, agents: Vec<Agent>) {
    let settings = match crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
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
            trust::tracked(trust, ctx, workspace, profile).await;
        }
    }
}

/// Trust the directory of a chat of agent `name` in `repository`, or
/// without one, at `home` as workspace launches trust theirs. Failure only
/// warns.
pub(in crate::cli) async fn trust_chat(
    ctx: &Context,
    settings: &Effective,
    name: &Agent,
    repository: Option<&Repository>,
    home: ChatHome<'_>,
) {
    if let Some(trust) = agent_integration(settings, name).trust_file() {
        let profile = &settings.launch(name).environment;
        trust::chat(trust, ctx, repository, home, profile).await;
    }
}

#[cfg(test)]
mod tests;
