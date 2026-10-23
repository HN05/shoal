//! Agents Shoal launches by name. A launch runs the agent's configured
//! command with the arguments its provider describes (see [`crate::ai`]); an
//! [`Integration`] adds what configuration cannot say for a tool Shoal knows,
//! such as hooks, trust entries or a launch of its own. Adding an agent means
//! configuration, plus an integration listed in [`INTEGRATIONS`] (or in
//! [`OPTIONAL`] behind its Cargo feature) when the tool needs one.
mod claude;
mod codex;
mod desktop;
#[cfg(feature = "happy")]
mod happy;
mod launch;
mod trust;

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use futures_util::future::BoxFuture;

use crate::{
    agent::Agent,
    cli::{
        client,
        commands::configured,
        context::Context,
        ui::{self, Fallback},
    },
    config::{Config, Effective, named_commands::Commands, templates::Delivery},
    fsutil,
    model::{RemovedWorkspace, Repository, Workspace},
    protocol::{ConfigTarget, Method},
    tools::Tool,
};

pub(super) use codex::shortcut as codex;
pub(super) use desktop::shortcut as open_app;
#[cfg(feature = "happy")]
pub(super) use happy::shortcut as happy;

/// What an agent needs beyond its configured command and described
/// arguments. Each default adds nothing.
pub(in crate::cli) trait Integration: Sync {
    /// Arguments that register Shoal's hooks for the agent's session.
    fn hook_args(&self) -> std::io::Result<Vec<OsString>> {
        Ok(Vec::new())
    }

    /// The hooks of a chat, which belongs to no workspace and so reports no
    /// turn state. They inherit the chat's identity.
    fn chat_hook_args(&self) -> std::io::Result<Vec<OsString>> {
        Ok(Vec::new())
    }

    /// How updates reach the agent launched under `settings`.
    fn delivery(&self, _ctx: &Context, _settings: &Effective) -> Delivery {
        Delivery::Watched
    }

    /// The config file in which the agent records trusted directories.
    fn trust_file(&self) -> Option<&'static trust::TrustFile> {
        None
    }

    /// Whether its wrapper wakes the idle agent by typing a prompt into its
    /// session, for agents without a hook that can start their turn.
    fn wakes_by_prompt(&self) -> bool {
        false
    }

    /// The programs a launch starts, when not its configured command's.
    fn programs(&self) -> Option<Vec<&'static str>> {
        None
    }

    /// Why the agent cannot run under `settings`, such as an integration
    /// turned off in config; pickers and completion leave it out.
    fn unavailable(&self, _settings: &Effective) -> Option<&'static str> {
        None
    }

    /// Start the agent its own way; `None` leaves it to the tracked terminal
    /// launch.
    fn launch<'a>(
        &'a self,
        _ctx: &'a Context,
        _launch: &'a mut Launch,
    ) -> BoxFuture<'a, Result<Option<i32>>> {
        Box::pin(async { Ok(None) })
    }
}

/// An agent without an integration: its configuration says everything.
struct Configured;

impl Integration for Configured {}

/// Integrations by the agent name they serve. A name not listed here needs
/// none.
const INTEGRATIONS: &[(&str, &dyn Integration)] =
    &[("claude", &claude::Claude), ("codex", &codex::Codex)];

/// Integrations built only with their Cargo feature.
#[cfg(feature = "happy")]
const OPTIONAL: &[(&str, &dyn Integration)] = &[
    ("happy-claude", &happy::CLAUDE),
    ("happy-codex", &happy::CODEX),
];
#[cfg(not(feature = "happy"))]
const OPTIONAL: &[(&str, &dyn Integration)] = &[];

fn integrations() -> impl Iterator<Item = (&'static str, &'static dyn Integration)> {
    INTEGRATIONS.iter().chain(OPTIONAL).copied()
}

fn integration(name: &str) -> &'static dyn Integration {
    integrations()
        .find(|(served, _)| *served == name)
        .map_or(&Configured, |(_, integration)| integration)
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
    /// Whether the agent must run tracked, never handed to a desktop app, as
    /// a detached start must.
    pub tracked: bool,
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

/// How a launch relates to the agents around it.
#[derive(Default)]
pub(in crate::cli) struct LaunchOptions {
    /// As [`Launch::tracked`].
    pub tracked: bool,
    /// As [`Launch::replaces`].
    pub replaces: Vec<PathBuf>,
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
    let integration = integration(agent.name());
    let mut launch = Launch {
        name: agent.into(),
        workspace,
        settings,
        prompt,
        args,
        tracked: options.tracked,
        replaces: options.replaces,
    };
    // A launch of its own returns once its agent has started.
    if let Some(code) = integration.launch(ctx, &mut launch).await? {
        for record in &launch.replaces {
            crate::execution::recovery::consume(record)?;
        }
        return Ok(code);
    }
    launch::tracked(ctx, integration, launch).await
}

/// `shoal <name>` and `shoal run <name>`: agents launch with their prompts,
/// other names run as plain configured commands.
pub(super) async fn run_named(
    ctx: &Context,
    name: &str,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let providers = crate::ai::providers(&Config::load(&ctx.paths)?.ai);
    if !providers.contains(name) && !integrations().any(|(served, _)| served == name) {
        return configured::run(ctx, name, workspace, args).await;
    }
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    launch_agent(
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
    for name in &settings.providers {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names.extend(
        integrations()
            .filter(|(name, integration)| {
                !names.iter().any(|known| known == name)
                    && integration.unavailable(settings).is_none()
            })
            .map(|(name, _)| name.to_owned())
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
    for (name, _) in integrations() {
        if !names.iter().any(|known| known == name) {
            names.push(name.to_owned());
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
    if let Some(reason) = integration(name).unavailable(settings) {
        anyhow::bail!("cannot start {name}: {reason}");
    }
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
    if let Some(programs) = integration(name).programs() {
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

/// Arguments that register Shoal's hooks for agent `name`'s session.
pub(crate) fn hook_args(name: &str) -> std::io::Result<Vec<OsString>> {
    integration(name).hook_args()
}

/// Arguments that register Shoal's hooks for a chat of agent `name`.
pub(in crate::cli) fn chat_hook_args(name: &str) -> std::io::Result<Vec<OsString>> {
    integration(name).chat_hook_args()
}

/// Whether agent `name`'s wrapper wakes it by typing a prompt.
pub(crate) fn wakes_by_prompt(name: &str) -> bool {
    integration(name).wakes_by_prompt()
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
        .map(|agent| agent.record.agent.as_str());
    let last = removed
        .last_session
        .iter()
        .filter(|last| last.session.is_some())
        .map(|last| last.agent.as_str());
    for trust in stopped
        .chain(last)
        .filter_map(|name| integration(name).trust_file())
    {
        trust.trust_tracked(ctx, workspace).await;
    }
}

/// Trust a repository's checkout for a chat of agent `name` as workspace
/// launches trust theirs. Failure only warns.
pub(in crate::cli) fn trust_checkout(ctx: &Context, name: &str, repository: &Repository) {
    if let Some(trust) = integration(name).trust_file() {
        trust.trust_checkout(ctx, repository);
    }
}

/// Remove every integration's trust entries for a repository after its
/// deletion succeeds, both where agents running with this process's
/// environment keep them and where the daemon kept that its tracked agents
/// ran with, which it then forgets. Cleanup is best effort and never fails
/// the removal.
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
        Method::AgentEnvironments {
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
        Method::ForgetAgentEnvironments {
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
    for (_, integration) in integrations() {
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
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::*;
    use crate::config::named_commands;

    #[test]
    fn agents_are_installed_only_when_every_program_they_start_is_on_path() {
        let bin = tempfile::tempdir().unwrap();
        for program in ["claude", "probe"] {
            let path = bin.path().join(program);
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut commands = named_commands::defaults();
        for (name, program) in [
            ("probe", "probe".to_owned()),
            ("missing", "missing".to_owned()),
            ("absolute", bin.path().join("probe").display().to_string()),
            ("relative", "./scripts/agent".to_owned()),
        ] {
            commands.insert(name.into(), vec![program]);
        }
        let search_path = bin.path().as_os_str();
        let installed = |name: &str| installed(name, &commands, search_path);

        assert!(installed("claude"));
        assert!(!installed("codex"));
        // Happy needs both its own CLI and the agent it runs.
        #[cfg(feature = "happy")]
        {
            assert!(!installed("happy-claude"));
            fs::copy(bin.path().join("claude"), bin.path().join("happy")).unwrap();
            assert!(installed("happy-claude"));
            assert!(!installed("happy-codex"));
        }

        assert!(installed("probe"));
        assert!(installed("absolute"));
        assert!(installed("relative"));
        assert!(!installed("missing"));
        assert!(!installed("undefined"));
    }

    #[test]
    fn launches_name_the_missing_program_or_undefined_agent() {
        let mut commands = named_commands::defaults();
        commands.insert("probe".into(), vec!["shoal-test-missing-program".into()]);
        let error = ensure_installed("probe", &commands).unwrap_err();
        assert_eq!(
            error.to_string(),
            "cannot start probe: shoal-test-missing-program is not installed or not on PATH"
        );
        let error = ensure_installed("undefined", &commands).unwrap_err();
        assert!(error.to_string().starts_with("unknown agent"), "{error}");
    }

    #[test]
    fn integrations_serve_only_the_names_they_list() {
        assert!(integration("claude").trust_file().is_some());
        assert!(integration("codex").wakes_by_prompt());
        assert!(!integration("claude").wakes_by_prompt());
        let configured = integration("droid");
        assert!(configured.trust_file().is_none());
        assert!(configured.hook_args().unwrap().is_empty());
        assert_eq!(
            builtin_names()[..2],
            ["codex".to_owned(), "claude".to_owned()]
        );
    }
}
