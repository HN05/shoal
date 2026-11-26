//! Shell completion. It runs before the normal CLI: read-only, scoped and
//! time-bounded.
use crate::paths::PathsExt;
use std::{
    collections::{BTreeSet, HashMap},
    ffi::{OsStr, OsString},
    future::Future,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::{Context as _, Result};
use clap::{Command, CommandFactory, FromArgMatches};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};

use crate::{
    agent::Agent,
    cli::{
        agents,
        ui::{self, Acting},
    },
    client::connection::CommandPaths,
    client::leases,
    config::{
        Config,
        modes::ModeName,
        named_commands::{self, Commands},
        repo::ConfigLayers,
        resolve::Stack,
    },
    env,
    model::{Chat, Workspace},
    paths::Paths,
    protocol::ConfigTarget,
    scoping::Scoping,
    workspace_context::{ScopeOrder, WorkspaceContext},
};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::model::Offer;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

#[derive(Default)]
struct Typed {
    state: Option<PathBuf>,
    workspace: Option<SpaceSelector>,
    pool: Option<String>,
    all: bool,
    restore: RestoreAction,
    /// Workspaces already named on the line, which are not offered again.
    named: Vec<String>,
    /// What the typed command sends for the space it acts on. The daemon judges
    /// the offered spaces by it.
    acting: Option<Acting>,
    settings: OnceLock<Settings>,
}

#[derive(Clone, Copy, Default)]
enum RestoreAction {
    /// Removed spaces, which it reopens.
    #[default]
    Reopen,
    /// An existing workspace, for `--checkpoint`.
    Checkpoint,
    /// Spaces whose agent can resume, for `--discard-agent`.
    Discard,
}

impl RestoreAction {
    fn of(matches: &clap::ArgMatches) -> Self {
        let given = |id: &str| matches.try_contains_id(id).unwrap_or(false);
        let discard = matches.try_get_one::<bool>("discard_agent").ok().flatten() == Some(&true);
        match (given("checkpoint"), discard) {
            (true, _) => Self::Checkpoint,
            (false, true) => Self::Discard,
            (false, false) => Self::Reopen,
        }
    }

    fn target(self) -> Target {
        match self {
            Self::Reopen => Target::RemovedSpaces,
            Self::Checkpoint => Target::Workspaces,
            Self::Discard => Target::ResumeSpaces,
        }
    }
}

/// The configuration completion reads, from the current directory when no
/// workspace resolves.
struct Settings {
    commands: Commands,
    /// The agents' launchers, whose programs decide which agents complete.
    launchers: Commands,
    /// Agents pickers offer, installed or not.
    agents: Vec<Agent>,
    modes: Vec<ModeName>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Commands,
    Repositories,
    Branches,
    /// Workspaces, each described by why the command would refuse it.
    Workspaces,
    Spaces,
    ResumeSpaces,
    RemovedSpaces,
    Pools,
    Members,
    ResourceNames,
    ApprovalRequests,
    Threads,
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
            leaf = child;
        }
        typed.workspace = space_value(leaf, "workspace").or_else(|| space_value(leaf, "space"));
        typed.named = leaf
            .try_get_many::<SpaceSelector>("workspace")
            .ok()
            .flatten()
            .or_else(|| leaf.try_get_many::<SpaceSelector>("space").ok().flatten())
            .map(|names| names.map(ToString::to_string).collect())
            .unwrap_or_default();
        typed.pool = value(leaf, "pool");
        typed.all = leaf.try_get_one::<bool>("all").ok().flatten() == Some(&true);
        if path == ["restore"] {
            typed.restore = RestoreAction::of(leaf);
        }
        typed.acting = typed_acting(&words);
    }
    // Decoration reads every command's arguments, which are deferred until built.
    command.build();
    decorate(command, &[], Arc::new(typed))
}

/// What the command in `words` sends for the space it acts on. None while
/// the words do not parse yet, such as while a required argument is missing.
fn typed_acting(words: &[OsString]) -> Option<Acting> {
    let typed = &words[..words.len().saturating_sub(1)];
    let command = crate::cli::Cli::command().ignore_errors(true);
    let matches = command.try_get_matches_from(typed).ok()?;
    let cli = crate::cli::Cli::from_arg_matches(&matches).ok()?;
    cli.command.as_ref().and_then(crate::cli::commands::acting)
}

fn value(matches: &clap::ArgMatches, id: &str) -> Option<String> {
    matches.try_get_one::<String>(id).ok().flatten().cloned()
}

fn space_value(matches: &clap::ArgMatches, id: &str) -> Option<SpaceSelector> {
    matches
        .try_get_one::<SpaceSelector>(id)
        .ok()
        .flatten()
        .cloned()
}

/// Attach live completers to the arguments that name daemon-managed objects.
/// `path` names `command` below the root.
fn decorate(command: Command, path: &[String], typed: Arc<Typed>) -> Command {
    let name = path.last().cloned().unwrap_or_default();
    let parent = match path {
        [.., parent, _] => parent.as_str(),
        _ => "",
    };
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
            let target = target_of(arg.get_id().as_str(), parent, name.as_str());
            if let Some(target) = target {
                let typed = typed.clone();
                let offer_all = offer_all
                    && !typed.all
                    && matches!(
                        target,
                        Target::Workspaces | Target::Spaces | Target::ResumeSpaces
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
    fn command_names(&self) -> Vec<String> {
        self.settings().commands.keys().cloned().collect()
    }

    /// Whether `agent`'s programs are on `search_path`, including the ones
    /// integrations add, such as Happy's.
    fn launchable(&self, agent: &Agent, search_path: &OsStr) -> bool {
        agents::installed(agent, &self.settings().launchers, search_path)
    }

    fn settings(&self) -> &Settings {
        self.settings.get_or_init(|| self.load_settings())
    }

    fn load_settings(&self) -> Settings {
        let state = self
            .state
            .clone()
            .or_else(|| std::env::var_os(env::STATE_DIR).map(PathBuf::from));
        let Ok(paths) = Paths::of_command(state).map(CommandPaths::from) else {
            return Settings {
                commands: Commands::new(),
                launchers: named_commands::defaults(),
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
                let workspaces = crate::client::spaces::list(&paths).await?.into_workspaces();
                let cwd = std::env::current_dir().ok();
                let context = WorkspaceContext::from_directory(&workspaces, cwd.as_deref());
                let workspace = context
                    .resolve(
                        self.workspace.as_ref(),
                        Scoping::of_workspace(),
                        ScopeOrder::AfterDirectory,
                    )
                    // Command names remain available from cwd while a typed
                    // workspace is incomplete or unknown.
                    .or_else(|| {
                        context.resolve(None, Scoping::Unscoped, ScopeOrder::AfterDirectory)
                    })
                    .context("no current workspace")?;
                let target = ConfigTarget::Space(SpaceSelector::id(&workspace.id));
                crate::client::config::config_layers(&paths, target).await
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
        let target = match target {
            Target::RemovedSpaces => self.restore.target(),
            target => target,
        };
        if matches!(target, Target::Commands) {
            return self
                .command_names()
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
            Target::Workspaces | Target::Spaces | Target::ResumeSpaces | Target::RemovedSpaces
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
        let paths = CommandPaths::from(Paths::of_command(state)?);
        if matches!(target, Target::Repositories) {
            return Ok(repository_candidates(
                &crate::client::repositories::list(&paths).await?,
                current,
            ));
        }
        if matches!(target, Target::Branches) {
            return branch_candidates().await;
        }
        if matches!(target, Target::ApprovalRequests) {
            let requests = crate::client::approvals::list(&paths, None).await?;
            return Ok(requests
                .iter()
                .filter(|r| r.status == shoal_protocol::approvals::DecisionStatus::Pending)
                .map(|r| {
                    CompletionCandidate::new(r.number.to_string())
                        .help(Some(super::commands::approval_summary(r).into()))
                })
                .collect());
        }
        if matches!(target, Target::Threads) {
            let open = shoal_protocol::threads::ThreadState::Open;
            let threads = crate::client::threads::list(&paths, None, open)
                .await?
                .threads;
            return Ok(threads
                .iter()
                .map(|thread| {
                    CompletionCandidate::new(thread.number.to_string())
                        .help(Some(super::commands::thread_summary(thread).into()))
                })
                .collect());
        }
        let workspaces = crate::client::spaces::list(&paths).await?.into_workspaces();
        if matches!(target, Target::Workspaces | Target::Spaces) {
            let offers = self.offers(&paths).await;
            let mut candidates: Vec<_> = (workspaces.iter())
                .filter_map(|w| {
                    let candidate = space_candidate(w.number, &w.name);
                    offered(candidate, w.name.to_string(), offers.get(&w.id))
                })
                .collect();
            if target == Target::Spaces {
                candidates.extend(chat_candidates(&paths, &offers).await?);
            }
            return Ok(candidates);
        }
        if matches!(target, Target::ResumeSpaces) {
            let summaries = ui::saved_summaries(&paths).await;
            let mut candidates: Vec<_> = workspaces
                .into_iter()
                .map(|w| {
                    let status = ui::saved_status(summaries.get(&w.id))
                        .map_or_else(|| ui::NOTHING_TO_RESUME.into(), |(text, _)| text);
                    let help = format!("{}: {status}", w.name);
                    space_candidate(w.number, &w.name).help(Some(help.into()))
                })
                .collect();
            candidates.extend(stopped_chat_candidates(
                crate::client::spaces::list(&paths).await?.into_chats(),
            ));
            return Ok(candidates);
        }
        if matches!(target, Target::RemovedSpaces) {
            let removed = crate::client::removal::removed(&paths).await?;
            return Ok((removed.into_iter())
                .map(|space| {
                    let help = format!("{} (removed)", space.name);
                    CompletionCandidate::new(space.space_id.to_string()).help(Some(help.into()))
                })
                .collect());
        }
        let workspace = self
            .workspace(&workspaces)
            .context("no current workspace")?
            .id
            .clone();
        let mut names = vec![];
        match target {
            Target::Pools | Target::Members | Target::ResourceNames => {
                let overview = leases::resources(SpaceSelector::id(&workspace))
                    .send(&paths)
                    .await?;
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
                    let catalog = leases::port_catalog(SpaceSelector::id(&workspace))
                        .send(&paths)
                        .await?;
                    names.extend(catalog.configured.into_keys());
                }
                let overview = leases::ports(SpaceSelector::id(&workspace))
                    .send(&paths)
                    .await?;
                names.extend(overview.reserved.into_iter().map(|p| p.name));
            }
            Target::SimNames => {
                let simulators =
                    leases::simulator_devices(&paths, Some(SpaceSelector::id(&workspace))).await?;
                names.extend(simulators.into_iter().filter_map(|s| s.lease_name));
            }
            Target::Commands
            | Target::Repositories
            | Target::Branches
            | Target::Workspaces
            | Target::Spaces
            | Target::ResumeSpaces
            | Target::RemovedSpaces
            | Target::ApprovalRequests
            | Target::Threads => unreachable!(),
        }
        Ok(names.into_iter().map(CompletionCandidate::new).collect())
    }

    /// The daemon's verdicts on this machine's spaces for the typed command's
    /// request. None, which offers every space, without a request or while the
    /// daemon cannot say.
    async fn offers(&self, paths: &CommandPaths) -> HashMap<SpaceId, Offer> {
        let Some(acting) = &self.acting else {
            return HashMap::new();
        };
        let judged = crate::client::spaces::judged_here(paths, acting.operation()).await;
        let judged = judged.unwrap_or_default().into_iter();
        judged.map(|judged| (judged.space, judged.offer)).collect()
    }

    /// The explicitly typed workspace, else the one containing the current
    /// directory, else the scoped execution's own workspace.
    fn workspace<'a>(&self, workspaces: &'a [Workspace]) -> Option<&'a Workspace> {
        let cwd = std::env::current_dir().ok();
        WorkspaceContext::from_directory(workspaces, cwd.as_deref()).resolve(
            self.workspace.as_ref(),
            Scoping::of_workspace(),
            ScopeOrder::AfterDirectory,
        )
    }
}

async fn live<T>(lookup: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(Duration::from_millis(500), lookup)
        .await
        .context("live completion lookup timed out")?
}

/// A space as completion offers it: its number, which selects it, with
/// its name, which does not, as its description.
fn space_candidate(number: SpaceNumber, name: &SpaceName) -> CompletionCandidate {
    CompletionCandidate::new(number.to_string()).help(Some(name.to_string().into()))
}

fn chat_candidate(chat: &Chat, kind: &str) -> CompletionCandidate {
    let help = format!("{} ({kind})", chat.name);
    CompletionCandidate::new(chat.number.to_string()).help(Some(help.into()))
}

/// `candidate` as `offer` shows it: described by why the command refuses
/// it, or left out where it lacks what the command acts on.
fn offered(
    candidate: CompletionCandidate,
    name: String,
    offer: Option<&Offer>,
) -> Option<CompletionCandidate> {
    match offer {
        None | Some(Offer::Accepted) => Some(candidate),
        Some(Offer::Refused { reason }) => {
            Some(candidate.help(Some(format!("{name}: {reason}").into())))
        }
        Some(Offer::Lacking) => None,
    }
}

async fn chat_candidates(
    paths: &CommandPaths,
    offers: &HashMap<SpaceId, Offer>,
) -> Result<Vec<CompletionCandidate>> {
    let chats = crate::client::spaces::list(paths).await?.into_chats();
    Ok((chats.iter())
        .filter_map(|chat| {
            let kind = match chat.running {
                false => STOPPED_CHAT,
                true => "chat",
            };
            let label = format!("{} ({kind})", chat.name);
            offered(chat_candidate(chat, kind), label, offers.get(&chat.id))
        })
        .collect())
}

fn stopped_chat_candidates(chats: Vec<Chat>) -> Vec<CompletionCandidate> {
    chats
        .iter()
        .filter(|chat| !chat.running)
        .map(|chat| chat_candidate(chat, STOPPED_CHAT))
        .collect()
}

const STOPPED_CHAT: &str = "stopped chat";

fn target_of(id: &str, parent: &str, name: &str) -> Option<Target> {
    match (id, parent, name) {
        ("id", _, "approvals") | ("id", "approvals", "withdraw") => Some(Target::ApprovalRequests),
        ("thread" | "of", "thread", _) => Some(Target::Threads),
        ("name", _, "run") => Some(Target::Commands),
        ("repository" | "repository_override" | "repo", _, _) => Some(Target::Repositories),
        ("branch", _, "start") => Some(Target::Branches),
        ("space", _, "resume") => Some(Target::ResumeSpaces),
        ("space", _, "restore") => Some(Target::RemovedSpaces),
        // Attaching reaches chats' sessions too.
        ("workspace", _, "attach") => Some(Target::Spaces),
        ("workspace", _, _) | ("in_workspace", _, "start") => Some(Target::Workspaces),
        ("parent", _, "start") => Some(Target::Spaces),
        ("space", _, _) => Some(Target::Spaces),
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

async fn branch_candidates() -> Result<Vec<CompletionCandidate>> {
    let root = crate::git::worktree_root(&std::env::current_dir()?).await?;
    let names = crate::git::branch::selectors(&root).await?;
    Ok(names.into_iter().map(CompletionCandidate::new).collect())
}

fn repository_candidates(
    repos: &[crate::model::Repository],
    current: &str,
) -> Vec<CompletionCandidate> {
    let mut candidates = vec![];
    for repo in repos {
        let name = repo.display_name();
        let unique =
            repo.name.is_some() || repos.iter().filter(|r| r.display_name() == name).count() == 1;
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

fn layered_settings(global: &Config, layers: &ConfigLayers) -> Settings {
    let commands: Commands = Stack::new(global, layers)
        .map(|stack| stack.named(|config| &config.commands))
        .unwrap_or_default()
        .into_iter()
        .map(|(name, (argv, _))| (name, argv))
        .collect();
    let resolved = global.resolve(layers);
    let launchers = match &resolved {
        Ok(settings) => settings.launchers.clone(),
        Err(_) => named_commands::defaults(),
    };
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
    // The target's agents, so a repository's own are agents here too.
    let agents = resolved.map_or_else(
        |_| {
            let tools = global.tools().unwrap_or_default();
            (crate::ai::agents(&tools))
                .filter_map(|name| name.parse().ok())
                .collect()
        },
        |settings| agents::names(&settings),
    );
    Settings {
        commands,
        launchers,
        agents,
        modes,
    }
}

#[cfg(test)]
mod tests;
