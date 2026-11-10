//! Completion runs before the normal CLI: read-only, scoped, and time bounded.
use crate::cli::availability::NeedExt;
use crate::paths::PathsExt;
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    future::Future,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::{Context as _, Result};
use clap::{Command, CommandFactory};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};

use crate::client::space_names::SpaceSelector;
use crate::{
    agent::Agent,
    cli::{
        agents,
        availability::{self, Fit, Need},
        terminal, ui,
        workspace_context::{ScopeOrder, WorkspaceContext},
    },
    client::leases,
    config::modes::ModeName,
    config::{
        Config,
        named_commands::{self, Commands},
        repo::ConfigLayers,
        resolve::Stack,
    },
    env,
    model::{Chat, Workspace},
    paths::Paths,
    protocol::ConfigTarget,
};

/// What was already typed on the command line when completion was requested.
#[derive(Default)]
struct Typed {
    state: Option<PathBuf>,
    workspace: Option<String>,
    pool: Option<String>,
    /// `--all` is already on the line.
    all: bool,
    /// Workspaces already named on the line, which are not offered again.
    named: Vec<String>,
    custom: Option<String>,
    /// The selected command also governs inherited global workspace options.
    need: Option<Need>,
    settings: OnceLock<Settings>,
}

/// The configuration completion reads, from cwd when no workspace resolves.
struct Settings {
    commands: Commands,
    providers: BTreeSet<String>,
    /// Agents pickers offer, installed or not.
    agents: Vec<Agent>,
    /// The modes `shoal start` takes.
    modes: Vec<ModeName>,
}

/// Live values an argument can be completed with.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Commands,
    Repositories,
    /// Workspaces, each described by why the command would refuse it.
    Workspaces(Need),
    /// Spaces: workspaces as [`Target::Workspaces`] offers them, then
    /// chats.
    Spaces(Need),
    /// Workspaces described by what `shoal resume` restores in them, then
    /// the stopped chats.
    ResumeSpaces,
    /// Removed workspaces `shoal restore` can open again, then existing
    /// ones it can return to a checkpoint.
    RestorableWorkspaces,
    Pools,
    Members,
    ResourceNames,
    ApprovalRequests,
    Questions,
    Ports,
    ReservedPorts,
    SimNames,
}

pub fn command() -> Command {
    let mut command = crate::cli::Cli::command();
    let words: Vec<_> = std::env::args_os()
        .skip_while(|s| s != "--")
        .skip(1)
        .collect();
    let mut typed = Typed::default();
    if !words.is_empty()
        && let Ok(matches) = command
            .clone()
            .ignore_errors(true)
            .try_get_matches_from(words.clone())
    {
        typed.state = matches
            .try_get_one::<PathBuf>("state_dir")
            .ok()
            .flatten()
            .cloned();
        let mut leaf = &matches;
        let mut path = Vec::new();
        while let Some((name, child)) = leaf.subcommand() {
            path.push(name);
            if child
                .try_get_many::<OsString>("")
                .ok()
                .flatten()
                .is_some_and(|args| args.len() > 0)
            {
                typed.custom = Some(name.to_owned());
            }
            leaf = child;
        }
        typed.need = Need::of_matches(&path, leaf);
        typed.workspace = space_value(leaf, "workspace")
            .or_else(|| space_value(leaf, "space"))
            .or_else(|| {
                leaf.try_get_many::<OsString>("")
                    .ok()
                    .flatten()
                    .and_then(|mut args| args.next())
                    .filter(|arg| !arg.to_string_lossy().starts_with('-'))
                    .and_then(|arg| arg.to_str().map(str::to_owned))
            });
        typed.named = leaf
            .try_get_many::<SpaceSelector>("workspace")
            .ok()
            .flatten()
            .or_else(|| leaf.try_get_many::<SpaceSelector>("space").ok().flatten())
            .map(|names| names.map(ToString::to_string).collect())
            .unwrap_or_default();
        typed.pool = value(leaf, "pool");
        typed.all = leaf.try_get_one::<bool>("all").ok().flatten() == Some(&true);
    }
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    for name in typed.command_names(&search_path) {
        if command
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == name)
        {
            continue;
        }
        let about = if typed
            .settings()
            .agents
            .iter()
            .any(|agent| *agent == name.as_str())
        {
            format!("Run {name} in a workspace")
        } else {
            "Configured workspace command".to_owned()
        };
        command = command.subcommand(
            Command::new(name)
                .about(about)
                .arg(clap::Arg::new("workspace"))
                .arg(clap::Arg::new("args").last(true).num_args(0..)),
        );
    }
    // Decoration reads every command's arguments, which are deferred until built.
    command.build();
    decorate(command, &[], Arc::new(typed))
}

fn value(matches: &clap::ArgMatches, id: &str) -> Option<String> {
    matches.try_get_one::<String>(id).ok().flatten().cloned()
}

/// The text of the space argument `id`, as typed.
fn space_value(matches: &clap::ArgMatches, id: &str) -> Option<String> {
    let selector = matches.try_get_one::<SpaceSelector>(id).ok().flatten();
    selector.map(ToString::to_string)
}

/// Attach live completers to the arguments that name daemon-managed objects.
/// `path` names `command` below the root.
fn decorate(command: Command, path: &[String], typed: Arc<Typed>) -> Command {
    let name = path.last().cloned().unwrap_or_default();
    let parent = match path {
        [.., parent, _] => parent.as_str(),
        _ => "",
    };
    let need = typed
        .need
        .unwrap_or_else(|| Need::of_command(&path.iter().map(String::as_str).collect::<Vec<_>>()));
    // A positional workspace or space argument beside `--all` offers it first
    // among the names, so the flag itself is not listed again after them. A
    // `--space` option never completes where a flag is typed, so `--all`
    // stays listed beside it.
    let positional = |id: &str| command.get_positionals().any(|arg| arg.get_id() == id);
    let has = |id: &str| command.get_arguments().any(|arg| arg.get_id() == id);
    let offer_all = (positional("workspace") || positional("space")) && has("all");
    command
        .mut_args(|arg| {
            let arg = own_first(arg);
            if offer_all && arg.get_id() == "all" {
                return arg.hide(true);
            }
            let target = target_of(arg.get_id().as_str(), parent, name.as_str(), need);
            if let Some(target) = target {
                let typed = typed.clone();
                let offer_all = offer_all
                    && !typed.all
                    && matches!(
                        target,
                        Target::Workspaces(_) | Target::Spaces(_) | Target::ResumeSpaces
                    );
                arg.add(ArgValueCompleter::new(move |current: &OsStr| {
                    let mut values = typed.complete(target, current);
                    if offer_all && "--all".starts_with(current.to_string_lossy().as_ref()) {
                        let all =
                            CompletionCandidate::new("--all").help(Some(ui::ALL_WORKSPACES.into()));
                        values.insert(0, all);
                    }
                    values
                }))
            } else if name == "start" && arg.get_id() == "mode" {
                let typed = typed.clone();
                arg.add(ArgValueCompleter::new(move |current: &OsStr| {
                    (typed.settings().modes.iter())
                        .filter(|mode| {
                            mode.as_str()
                                .starts_with(current.to_string_lossy().as_ref())
                        })
                        .map(|mode| CompletionCandidate::new(mode.as_str()))
                        .collect::<Vec<_>>()
                }))
            } else if (name == "start" || (parent == "review" && name == "run"))
                && arg.get_id() == "agent"
            {
                let typed = typed.clone();
                arg.add(ArgValueCompleter::new(move |current: &OsStr| {
                    let search_path = std::env::var_os("PATH").unwrap_or_default();
                    typed
                        .settings()
                        .agents
                        .iter()
                        .filter(|agent| {
                            agent.name().starts_with(current.to_string_lossy().as_ref())
                        })
                        .filter(|name| typed.launchable(name, &search_path))
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .map(CompletionCandidate::new)
                        .collect::<Vec<_>>()
                }))
            } else if parent == "skill" && name == "install" && arg.get_id() == "agent" {
                arg.add(ArgValueCompleter::new(|current: &OsStr| {
                    let mut names = vec!["all".to_owned()];
                    if let Ok(home) = crate::fsutil::home::home_dir()
                        && let Ok(agents) = crate::ai::load(&home)
                        && let Ok(directories) = crate::ai::skill_dirs(&agents, &home)
                    {
                        names.extend(
                            directories
                                .into_iter()
                                .filter_map(|(name, directory)| directory.map(|_| name)),
                        );
                    }
                    names
                        .into_iter()
                        .filter(|name| name.starts_with(current.to_string_lossy().as_ref()))
                        .map(CompletionCandidate::new)
                        .collect::<Vec<_>>()
                }))
            } else if parent == "repo" && name == "add" && arg.get_id() == "source" {
                arg.value_hint(clap::ValueHint::DirPath)
            } else {
                arg
            }
        })
        .mut_subcommands(|subcommand| {
            let mut path = path.to_vec();
            path.push(subcommand.get_name().to_owned());
            decorate(subcommand, &path, typed.clone())
        })
}

/// A command's deferred arguments follow the global ones its parent added
/// first; doubling the display order puts each own argument before a global
/// one it ties with.
fn own_first(arg: clap::Arg) -> clap::Arg {
    let order = 2 * arg.get_display_order() + usize::from(arg.is_global_set());
    arg.display_order(order)
}

impl Typed {
    /// The configured commands and the agents a command line names: those
    /// pickers offer, including the ones integrations add, such as Happy's,
    /// whose programs are on `search_path`.
    fn command_names(&self, search_path: &OsStr) -> Vec<String> {
        let settings = self.settings();
        let commands = (self.commands().keys()).filter(|name| !settings.providers.contains(*name));
        let agents = (settings.agents.iter())
            .filter(|agent| self.launchable(agent, search_path))
            .map(|agent| agent.name().to_owned());
        let mut names: BTreeSet<String> = commands.cloned().chain(agents).collect();
        names.extend(self.custom.clone());
        names.into_iter().collect()
    }

    fn commands(&self) -> &Commands {
        &self.settings().commands
    }

    /// Whether `agent`'s programs are on `search_path`.
    fn launchable(&self, agent: &Agent, search_path: &OsStr) -> bool {
        agents::installed(agent, self.commands(), search_path)
    }

    fn settings(&self) -> &Settings {
        self.settings.get_or_init(|| self.load_settings())
    }

    fn load_settings(&self) -> Settings {
        let state = self
            .state
            .clone()
            .or_else(|| std::env::var_os(env::STATE_DIR).map(PathBuf::from));
        let Ok(paths) = Paths::of_command(state) else {
            return Settings {
                commands: named_commands::defaults(),
                providers: crate::ai::agents(&crate::ai::tools(&Default::default()))
                    .map(str::to_owned)
                    .collect(),
                agents: crate::ai::PROVIDERS
                    .iter()
                    .filter_map(|provider| provider.name.parse().ok())
                    .collect(),
                modes: (crate::config::modes::built_in_modes().into_keys())
                    .map(ModeName::from)
                    .collect(),
            };
        };
        // Modes and rules may be defined only by the files beside it.
        let global = Config::load_with_templates(&paths).unwrap_or_default();
        let mut layers = ConfigLayers::default();
        if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            let layer = runtime.block_on(live(async {
                let workspaces = crate::client::workspaces::list(&paths).await?;
                let cwd = std::env::current_dir().ok();
                let context = WorkspaceContext::from_directory(&workspaces, cwd.as_deref());
                let workspace = context
                    .resolve(
                        self.workspace.as_deref().map(SpaceSelector::parse).as_ref(),
                        env::workspace_scoped(),
                        ScopeOrder::AfterDirectory,
                    )
                    // Command names remain available from cwd while a typed
                    // workspace is incomplete or unknown.
                    .or_else(|| context.resolve(None, false, ScopeOrder::AfterDirectory))
                    .context("no current workspace")?;
                let target = ConfigTarget::Workspace(SpaceSelector::id(&workspace.id));
                crate::client::workspaces::config_layers(&paths, target).await
            }));
            if let Ok(found) = layer {
                layers = found;
            }
        }
        layered_settings(&global, &layers)
    }

    fn complete(&self, target: Target, current: &OsStr) -> Vec<CompletionCandidate> {
        let Some(current) = current.to_str() else {
            return vec![];
        };
        if matches!(target, Target::Commands) {
            let search_path = std::env::var_os("PATH").unwrap_or_default();
            return self
                .command_names(&search_path)
                .into_iter()
                .filter(|name| name.starts_with(current))
                .map(CompletionCandidate::new)
                .collect();
        }
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return vec![];
        };
        let values = runtime.block_on(live(self.candidates(target, current)));
        let Ok(mut values) = values else {
            return vec![];
        };
        let workspaces = matches!(
            target,
            Target::Workspaces(_) | Target::Spaces(_) | Target::ResumeSpaces
        );
        values.retain(|value| {
            value.get_value().to_str().is_some_and(|s| {
                s.starts_with(current)
                    && !s.chars().any(char::is_control)
                    && !(workspaces && s != current && self.named.iter().any(|name| name == s))
            })
        });
        values.sort_by(|a, b| a.get_value().cmp(b.get_value()));
        values.dedup_by(|a, b| a.get_value() == b.get_value());
        values
    }

    async fn candidates(&self, target: Target, current: &str) -> Result<Vec<CompletionCandidate>> {
        let state = self
            .state
            .clone()
            .or_else(|| std::env::var_os(env::STATE_DIR).map(PathBuf::from));
        let paths = Paths::of_command(state)?;
        if matches!(target, Target::Repositories) {
            return Ok(repository_candidates(
                &crate::client::repositories::list(&paths).await?,
                current,
            ));
        }
        if matches!(target, Target::RestorableWorkspaces) {
            let removed = crate::client::removal::removed(&paths).await?;
            let existing = crate::client::workspaces::list(&paths).await?;
            return Ok(removed
                .into_iter()
                .map(|workspace| workspace.name)
                .chain(existing.into_iter().map(|workspace| workspace.name))
                .map(CompletionCandidate::new)
                .collect());
        }
        if matches!(target, Target::ApprovalRequests) {
            let requests = crate::client::approvals::list(&paths, None).await?;
            return Ok(requests
                .iter()
                .filter(|r| r.status == crate::client::approvals::DecisionStatus::Pending)
                .map(|r| {
                    CompletionCandidate::new(r.number.to_string())
                        .help(Some(super::commands::approval_summary(r).into()))
                })
                .collect());
        }
        if matches!(target, Target::Questions) {
            let questions = crate::client::questions::list(&paths, None)
                .await?
                .questions;
            return Ok(questions
                .iter()
                .map(|q| {
                    CompletionCandidate::new(q.number.to_string())
                        .help(Some(super::commands::question_summary(q).into()))
                })
                .collect());
        }
        let workspaces = crate::client::workspaces::list(&paths).await?;
        if let Target::Workspaces(need) | Target::Spaces(need) = target {
            let availability = availability::load(need, &paths, &workspaces).await;
            let mut candidates: Vec<_> = workspaces
                .iter()
                .filter_map(|w| {
                    let candidate = CompletionCandidate::new(&w.name);
                    match availability.fit(w) {
                        Fit::Accepted => Some(candidate),
                        Fit::Refused(reason) => Some(candidate.help(Some(reason.into()))),
                        Fit::Hidden => None,
                    }
                })
                .collect();
            // Attaching reaches running chats' sessions too.
            if need == Need::Session {
                candidates.extend(session_chat_candidates(&paths).await?);
            } else if matches!(target, Target::Spaces(_)) {
                candidates.extend(chat_candidates(&paths).await?);
            }
            return Ok(candidates);
        }
        if matches!(target, Target::ResumeSpaces) {
            let mut candidates: Vec<_> = workspaces
                .into_iter()
                .map(|w| {
                    let status = ui::resumable(&paths, &w.id)
                        .map_or_else(|| ui::NOTHING_TO_RESUME.into(), |(text, _)| text);
                    CompletionCandidate::new(&w.name).help(Some(status.into()))
                })
                .collect();
            candidates.extend(stopped_chat_candidates(
                crate::client::chats::recorded(&paths).await?,
            ));
            return Ok(candidates);
        }
        let workspace = self
            .workspace(&workspaces)
            .context("no current workspace")?
            .id
            .clone();
        let mut names = vec![];
        match target {
            Target::Pools | Target::Members | Target::ResourceNames => {
                let overview = leases::resources(&paths, SpaceSelector::id(&workspace)).await?;
                match target {
                    Target::Pools => names.extend(overview.pools.into_iter().map(|p| p.name)),
                    Target::Members => names.extend(
                        overview
                            .pools
                            .into_iter()
                            .filter(|p| Some(&p.name) == self.pool.as_ref())
                            .flat_map(|p| p.resources.into_iter().map(|r| r.name)),
                    ),
                    _ => names.extend(
                        overview
                            .leases
                            .into_iter()
                            .filter(|l| Some(&l.pool) == self.pool.as_ref())
                            .map(|l| l.name),
                    ),
                }
            }
            Target::Ports | Target::ReservedPorts => {
                if matches!(target, Target::Ports) {
                    let catalog =
                        leases::port_catalog(&paths, SpaceSelector::id(&workspace)).await?;
                    names.extend(catalog.configured.into_keys());
                }
                let overview = leases::ports(&paths, SpaceSelector::id(&workspace)).await?;
                names.extend(overview.reserved.into_iter().map(|p| p.name));
            }
            Target::SimNames => {
                let simulators =
                    leases::simulator_devices(&paths, Some(SpaceSelector::id(&workspace))).await?;
                names.extend(simulators.into_iter().filter_map(|s| s.lease_name));
            }
            Target::Commands
            | Target::Repositories
            | Target::Workspaces(_)
            | Target::Spaces(_)
            | Target::ResumeSpaces
            | Target::RestorableWorkspaces
            | Target::ApprovalRequests
            | Target::Questions => unreachable!(),
        }
        Ok(names.into_iter().map(CompletionCandidate::new).collect())
    }

    /// The explicitly typed workspace, else the one containing the current
    /// directory, else the scoped execution's own workspace.
    fn workspace<'a>(&self, workspaces: &'a [Workspace]) -> Option<&'a Workspace> {
        let cwd = std::env::current_dir().ok();
        WorkspaceContext::from_directory(workspaces, cwd.as_deref()).resolve(
            self.workspace.as_deref().map(SpaceSelector::parse).as_ref(),
            env::workspace_scoped(),
            ScopeOrder::AfterDirectory,
        )
    }
}

/// Apply the interactive deadline to a live lookup.
async fn live<T>(lookup: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(Duration::from_millis(500), lookup)
        .await
        .context("live completion lookup timed out")?
}

/// Every chat, by name, a stopped one described as such.
async fn chat_candidates(paths: &Paths) -> Result<Vec<CompletionCandidate>> {
    Ok(crate::client::chats::recorded(paths)
        .await?
        .into_iter()
        .map(|chat| {
            let help = match chat.restore {
                Some(_) => STOPPED_CHAT,
                None => "chat",
            };
            CompletionCandidate::new(chat.name).help(Some(help.into()))
        })
        .collect())
}

/// The stopped chats among `chats`, which `shoal resume` restores, by
/// name.
fn stopped_chat_candidates(chats: Vec<Chat>) -> Vec<CompletionCandidate> {
    chats
        .into_iter()
        .filter(|chat| chat.restore.is_some())
        .map(|chat| CompletionCandidate::new(chat.name).help(Some(STOPPED_CHAT.into())))
        .collect()
}

/// How completion describes a stopped chat.
const STOPPED_CHAT: &str = "stopped chat";

/// What the argument `id` of the command `name`, under `parent`, completes,
/// given what the command needs of a workspace.
fn target_of(id: &str, parent: &str, name: &str, need: Need) -> Option<Target> {
    match (id, parent, name) {
        ("id", _, "approvals") | ("id", "approvals", "withdraw") => Some(Target::ApprovalRequests),
        ("id", _, "questions") | ("id", "questions", "withdraw") => Some(Target::Questions),
        ("name", _, "run") => Some(Target::Commands),
        ("repository" | "repository_override" | "repo", _, _) => Some(Target::Repositories),
        ("workspace" | "space", _, "resume") => Some(Target::ResumeSpaces),
        ("workspace", _, "restore") => Some(Target::RestorableWorkspaces),
        ("workspace", _, _) | ("in_workspace", _, "start") => Some(Target::Workspaces(need)),
        ("parent", _, "start") => Some(Target::Spaces(need)),
        // Only stopping untracked processes takes chats.
        ("space", _, "stop") if need != Need::Untracked => Some(Target::Workspaces(need)),
        ("space", _, _) => Some(Target::Spaces(need)),
        ("name", "acquire", "port") => Some(Target::Ports),
        ("pool", "acquire" | "release", "resource" | "repo") | ("resource", "acquire", "repo") => {
            Some(Target::Pools)
        }
        ("member", "acquire", "resource") => Some(Target::Members),
        ("name", "release", "port") => Some(Target::ReservedPorts),
        ("name", "release", "sim") => Some(Target::SimNames),
        ("lease", "release", _) => Some(Target::ResourceNames),
        _ => None,
    }
}

/// The running chats with an agent session, by name.
async fn session_chat_candidates(paths: &Paths) -> Result<Vec<CompletionCandidate>> {
    let chats = crate::client::chats::list(paths).await?;
    let places = super::commands::sessions::chat_places(&chats);
    let attachable = terminal::session_owners(&places);
    Ok(chats
        .into_iter()
        .filter(|chat| attachable.contains(chat.id.as_str()))
        .map(|chat| CompletionCandidate::new(chat.name).help(Some("chat".into())))
        .collect())
}

fn repository_candidates(
    repos: &[crate::model::Repository],
    current: &str,
) -> Vec<CompletionCandidate> {
    let mut candidates = vec![];
    for repo in repos {
        let name = crate::repository::name(repo);
        let unique = repo.name.is_some()
            || repos
                .iter()
                .filter(|r| crate::repository::name(r) == name)
                .count()
                == 1;
        let target = if unique {
            name.to_owned()
        } else {
            repo.path.to_string_lossy().into_owned()
        };
        candidates.push(CompletionCandidate::new(target).help(Some(repo.source.clone().into())));
        if !current.is_empty() {
            for alternate in [
                &repo.id.to_string(),
                &repo.source,
                &repo.path.to_string_lossy().into_owned(),
            ] {
                if alternate.starts_with(current) {
                    candidates.push(
                        CompletionCandidate::new(alternate.clone())
                            .help(Some(name.to_owned().into())),
                    );
                }
            }
        }
    }
    candidates
}

/// What completion offers under `global` and the target's `layers`: a
/// repository's own agents count as agents too.
fn layered_settings(global: &Config, layers: &ConfigLayers) -> Settings {
    // Agents' launchers complete beside the workspace shortcuts.
    let mut commands: Commands = Stack::new(global, layers)
        .named(|config| &config.commands)
        .into_iter()
        .map(|(name, (argv, _))| (name, argv))
        .collect();
    let resolved = global.resolve(layers);
    // The target's agents, so a repository's own are agents here too.
    let tools = match &resolved {
        Ok(settings) => settings.tools.clone(),
        Err(_) => global.tools().unwrap_or_default(),
    };
    let providers: BTreeSet<_> = crate::ai::agents(&tools).map(str::to_owned).collect();
    if let Ok(settings) = &resolved {
        commands.extend(settings.launchers.clone());
    }
    let modes = match &resolved {
        Ok(settings) => settings
            .modes
            .keys()
            .map(|name| name.as_str().into())
            .collect(),
        Err(_) => (crate::config::modes::built_in_modes().into_keys())
            .map(ModeName::from)
            .collect(),
    };
    let agents = resolved.map_or_else(
        |_| {
            providers
                .iter()
                .filter_map(|name| name.parse().ok())
                .collect()
        },
        |settings| agents::names(&settings),
    );
    Settings {
        commands,
        providers,
        agents,
        modes,
    }
}

#[cfg(test)]
mod tests;
