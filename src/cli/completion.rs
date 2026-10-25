//! Completion runs before the normal CLI: read-only, scoped, and time bounded.
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::{Context as _, Result};
use clap::{Command, CommandFactory};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};

use crate::{
    cli::{
        agents,
        availability::{Availability, Fit, Need},
        client, ui,
        workspace_context::{ScopeOrder, WorkspaceContext},
    },
    config::{
        Config,
        named_commands::{self, Commands},
        repo::ConfigLayers,
        resolve::Stack,
    },
    daemon::{approvals::ApprovalRequest, questions::Listing, resources::Overview},
    env,
    model::{PortOverview, Workspace},
    paths::Paths,
    protocol::{ConfigTarget, Method},
    sim::Simulator,
};

/// What was already typed on the command line when completion was requested.
#[derive(Default)]
struct Typed {
    state: Option<PathBuf>,
    workspace: Option<String>,
    pool: Option<String>,
    /// `--all` is already on the line.
    all: bool,
    custom: Option<String>,
    settings: OnceLock<Settings>,
}

/// The configuration completion reads, from cwd when no workspace resolves.
struct Settings {
    commands: Commands,
    providers: BTreeSet<String>,
    /// Agents pickers offer, installed or not.
    agents: Vec<String>,
}

/// Live values an argument can be completed with.
#[derive(Clone, Copy)]
enum Target {
    Commands,
    Repositories,
    /// Workspaces, each described by why the command would refuse it.
    Workspaces(Need),
    /// Workspaces described by what `shoal resume` restores in them.
    ResumeWorkspaces,
    /// Removed workspaces `shoal work restore` can open again, then existing
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
        while let Some((name, child)) = leaf.subcommand() {
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
        typed.workspace = value(leaf, "workspace").or_else(|| {
            leaf.try_get_many::<OsString>("")
                .ok()
                .flatten()
                .and_then(|mut args| args.next())
                .filter(|arg| !arg.to_string_lossy().starts_with('-'))
                .and_then(|arg| arg.to_str().map(str::to_owned))
        });
        typed.pool = value(leaf, "pool");
        typed.all = leaf.try_get_one::<bool>("all").ok().flatten() == Some(&true);
    }
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    for name in typed.command_names() {
        if command
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == name)
        {
            continue;
        }
        let about = if !typed.settings().providers.contains(&name) {
            "Configured workspace command".to_owned()
        } else if typed.launchable(&name, &search_path) {
            format!("Run {name} in a workspace")
        } else {
            continue;
        };
        command = command.subcommand(
            Command::new(name)
                .about(about)
                .arg(clap::Arg::new("workspace"))
                .arg(clap::Arg::new("args").last(true).num_args(0..)),
        );
    }
    for (name, available) in agents::shortcuts(&typed.installed(&search_path)) {
        if !available {
            command = withdraw(command, name);
        }
    }
    decorate(command, &[], Arc::new(typed))
}

/// Keep a launcher whose executable is missing out of completion. Hidden
/// subcommands still complete as the only match, so the name also changes to
/// one no typed word can start. A launcher this build leaves out has nothing
/// to withdraw.
fn withdraw(command: Command, name: &str) -> Command {
    if command.find_subcommand(name).is_none() {
        return command;
    }
    command.mut_subcommand(name, |subcommand| {
        subcommand.name(format!("\0{name}")).hide(true)
    })
}

fn value(matches: &clap::ArgMatches, id: &str) -> Option<String> {
    matches.try_get_one::<String>(id).ok().flatten().cloned()
}

/// Attach live completers to the arguments that name daemon-managed objects.
/// `path` names `command` below the root.
fn decorate(command: Command, path: &[String], typed: Arc<Typed>) -> Command {
    let name = path.last().cloned().unwrap_or_default();
    let parent = match path {
        [.., parent, _] => parent.as_str(),
        _ => "",
    };
    let need = Need::of_command(&path.iter().map(String::as_str).collect::<Vec<_>>());
    // A workspace argument beside `--all` offers it first among the names,
    // so the flag itself is not listed again after them.
    let offer_all = ["workspace", "all"]
        .iter()
        .all(|id| command.get_arguments().any(|arg| arg.get_id() == id));
    command
        .mut_args(|arg| {
            if offer_all && arg.get_id() == "all" {
                return arg.hide(true);
            }
            let target = match (arg.get_id().as_str(), parent, name.as_str()) {
                ("id", _, "approvals") | ("id", "approvals", "withdraw") => {
                    Some(Target::ApprovalRequests)
                }
                ("id", _, "answer") | ("id", "questions", "withdraw") => Some(Target::Questions),
                ("name", _, "run") => Some(Target::Commands),
                ("repository" | "repository_override", _, _) => Some(Target::Repositories),
                ("workspace", _, "resume") => Some(Target::ResumeWorkspaces),
                ("workspace", _, "restore") => Some(Target::RestorableWorkspaces),
                ("workspace", _, _) => Some(Target::Workspaces(need)),
                ("pool", "resource", _) => Some(Target::Pools),
                ("resource", "resource", _) => Some(Target::Members),
                ("name", "resource", "release") => Some(Target::ResourceNames),
                ("name", "port", "acquire") => Some(Target::Ports),
                ("name", "port", "release") => Some(Target::ReservedPorts),
                ("name", "sim", "release") => Some(Target::SimNames),
                ("name", "acquire", "port") => Some(Target::Ports),
                ("pool", "acquire" | "release", "resource" | "repo")
                | ("resource", "acquire", "repo") => Some(Target::Pools),
                ("member", "acquire", "resource") => Some(Target::Members),
                ("name", "release", "port") => Some(Target::ReservedPorts),
                ("name", "release", "sim") => Some(Target::SimNames),
                ("lease", "release", _) => Some(Target::ResourceNames),
                _ => None,
            };
            if let Some(target) = target {
                let typed = typed.clone();
                let offer_all = offer_all
                    && !typed.all
                    && matches!(target, Target::Workspaces(_) | Target::ResumeWorkspaces);
                arg.add(ArgValueCompleter::new(move |current: &OsStr| {
                    let mut values = typed.complete(target, current);
                    if offer_all && "--all".starts_with(current.to_string_lossy().as_ref()) {
                        let all =
                            CompletionCandidate::new("--all").help(Some(ui::ALL_WORKSPACES.into()));
                        values.insert(0, all);
                    }
                    values
                }))
            } else if matches!(name.as_str(), "add" | "review") && arg.get_id() == "agent" {
                let typed = typed.clone();
                arg.add(ArgValueCompleter::new(move |current: &OsStr| {
                    let search_path = std::env::var_os("PATH").unwrap_or_default();
                    typed
                        .settings()
                        .agents
                        .iter()
                        .filter(|name| name.starts_with(current.to_string_lossy().as_ref()))
                        .filter(|name| typed.launchable(name, &search_path))
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .map(CompletionCandidate::new)
                        .collect::<Vec<_>>()
                }))
            } else if name == "happy" && arg.get_id() == "agent" {
                let typed = typed.clone();
                arg.add(ArgValueCompleter::new(move |current: &OsStr| {
                    let search_path = std::env::var_os("PATH").unwrap_or_default();
                    agents::happy_agents(&typed.installed(&search_path))
                        .into_iter()
                        .filter(|agent| agent.starts_with(current.to_string_lossy().as_ref()))
                        .map(CompletionCandidate::new)
                        .collect::<Vec<_>>()
                }))
            } else if parent == "skill" && name == "install" && arg.get_id() == "agent" {
                arg.add(ArgValueCompleter::new(|current: &OsStr| {
                    let mut names = vec!["all".to_owned()];
                    if let Ok(home) = crate::fsutil::home_dir()
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

impl Typed {
    fn command_names(&self) -> Vec<String> {
        let mut names: BTreeSet<String> = self.commands().keys().cloned().collect();
        names.extend(self.custom.clone());
        names.into_iter().collect()
    }

    fn commands(&self) -> &Commands {
        &self.settings().commands
    }

    /// Whether `name` is an agent whose programs are on `search_path`.
    fn launchable(&self, name: &str, search_path: &OsStr) -> bool {
        agents::installed(name, self.commands(), search_path)
    }

    /// The offered agents and commands, judged against `search_path`.
    fn installed<'a>(&'a self, search_path: &'a OsStr) -> agents::Installed<'a> {
        agents::Installed {
            agents: &self.settings().agents,
            commands: self.commands(),
            search_path,
        }
    }

    fn settings(&self) -> &Settings {
        self.settings.get_or_init(|| self.load_settings())
    }

    fn load_settings(&self) -> Settings {
        let state = self
            .state
            .clone()
            .or_else(|| std::env::var_os(env::STATE_DIR).map(PathBuf::from));
        let Ok(paths) = Paths::new(state) else {
            return Settings {
                commands: named_commands::defaults(),
                providers: crate::ai::providers(&Default::default()),
                agents: crate::ai::PROVIDERS
                    .iter()
                    .map(|provider| provider.name.to_owned())
                    .collect(),
            };
        };
        let global = Config::load(&paths).unwrap_or_default();
        let mut layers = ConfigLayers::default();
        if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            let layer = runtime.block_on(async {
                tokio::time::timeout(Duration::from_millis(500), async {
                    let workspaces = client::workspaces(&paths).await?;
                    let cwd = std::env::current_dir().ok();
                    let context = WorkspaceContext::from_directory(&workspaces, cwd.as_deref());
                    let workspace = context
                        .resolve(
                            self.workspace.as_deref(),
                            env::is_scoped(),
                            ScopeOrder::AfterDirectory,
                        )
                        // Command names remain available from cwd while a typed
                        // workspace is incomplete or unknown.
                        .or_else(|| context.resolve(None, false, ScopeOrder::AfterDirectory))
                        .context("no current workspace")?;
                    client::request::<Box<ConfigLayers>>(
                        &paths,
                        Method::LayeredConfig {
                            target: ConfigTarget::Workspace(workspace.id.clone()),
                        },
                    )
                    .await
                })
                .await
            });
            if let Ok(Ok(found)) = layer {
                layers = *found;
            }
        }
        let commands = Stack::new(&global, &layers)
            .named(|config| &config.commands)
            .into_iter()
            .map(|(name, (argv, _))| (name, argv))
            .collect();
        let providers = crate::ai::providers(&global.ai);
        let agents = global.resolve(&layers).map_or_else(
            |_| providers.iter().cloned().collect(),
            |settings| agents::names(&settings),
        );
        Settings {
            commands,
            providers,
            agents,
        }
    }

    fn complete(&self, target: Target, current: &OsStr) -> Vec<CompletionCandidate> {
        let Some(current) = current.to_str() else {
            return vec![];
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
        let values = runtime.block_on(async {
            tokio::time::timeout(Duration::from_millis(500), self.candidates(target, current)).await
        });
        let Ok(Ok(mut values)) = values else {
            return vec![];
        };
        values.retain(|value| {
            value
                .get_value()
                .to_str()
                .is_some_and(|s| s.starts_with(current) && !s.chars().any(char::is_control))
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
        let paths = Paths::new(state)?;
        if matches!(target, Target::Repositories) {
            return Ok(repository_candidates(
                &client::repositories(&paths).await?,
                current,
            ));
        }
        if matches!(target, Target::RestorableWorkspaces) {
            let removed =
                client::pages::<crate::model::RemovedWorkspace>(&paths, Method::RemovedWorkspaces)
                    .await?;
            let existing = client::workspaces(&paths).await?;
            return Ok(removed
                .into_iter()
                .map(|workspace| workspace.name)
                .chain(existing.into_iter().map(|workspace| workspace.name))
                .map(CompletionCandidate::new)
                .collect());
        }
        if matches!(target, Target::ApprovalRequests) {
            let requests = client::request::<Vec<ApprovalRequest>>(
                &paths,
                Method::ListApprovals { workspace: None },
            )
            .await?;
            return Ok(requests
                .iter()
                .filter(|r| r.status == crate::daemon::approvals::DecisionStatus::Pending)
                .map(|r| {
                    CompletionCandidate::new(r.id.to_string())
                        .help(Some(super::commands::approval_summary(r).into()))
                })
                .collect());
        }
        if matches!(target, Target::Questions) {
            let method = Method::ListQuestions { workspace: None };
            let questions = client::request::<Listing>(&paths, method).await?.questions;
            return Ok(questions
                .iter()
                .map(|q| {
                    CompletionCandidate::new(q.id.to_string())
                        .help(Some(super::commands::question_summary(q).into()))
                })
                .collect());
        }
        let workspaces = client::workspaces(&paths).await?;
        if let Target::Workspaces(need) = target {
            let availability = Availability::load(need, &paths, &workspaces).await;
            return Ok(workspaces
                .iter()
                .filter_map(|w| {
                    let candidate = CompletionCandidate::new(&w.name);
                    match availability.fit(w) {
                        Fit::Accepted => Some(candidate),
                        Fit::Refused(reason) => Some(candidate.help(Some(reason.into()))),
                        Fit::Hidden => None,
                    }
                })
                .collect());
        }
        if matches!(target, Target::ResumeWorkspaces) {
            return Ok(workspaces
                .into_iter()
                .map(|w| {
                    let status = ui::resumable(&paths, &w.id)
                        .map_or_else(|| ui::NOTHING_TO_RESUME.into(), |(text, _)| text);
                    CompletionCandidate::new(&w.name).help(Some(status.into()))
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
                let overview =
                    client::request::<Overview>(&paths, Method::ResourceOverview { workspace })
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
                let overview =
                    client::request::<PortOverview>(&paths, Method::PortOverview { workspace })
                        .await?;
                if matches!(target, Target::Ports) {
                    names.extend(overview.configured.into_keys());
                }
                names.extend(overview.reserved.into_iter().map(|p| p.name));
            }
            Target::SimNames => {
                let method = Method::SimList {
                    workspace: Some(workspace),
                };
                let simulators = client::request::<Vec<Simulator>>(&paths, method).await?;
                names.extend(simulators.into_iter().filter_map(|s| s.lease_name));
            }
            Target::Commands
            | Target::Repositories
            | Target::Workspaces(_)
            | Target::ResumeWorkspaces
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
            self.workspace.as_deref(),
            env::is_scoped(),
            ScopeOrder::AfterDirectory,
        )
    }
}

/// Unique display names, with IDs, sources, and paths as prefix-matched extras.
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
                &repo.id,
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
