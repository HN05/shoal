//! Completion runs before the normal CLI: read-only, scoped, and time bounded.
use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::{Context as _, Result};
use clap::{Command, CommandFactory};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};

use crate::{
    client,
    config::Config,
    env,
    model::Workspace,
    paths::Paths,
    protocol::{Body, ConfigTarget, Method},
};

/// What was already typed on the command line when completion was requested.
#[derive(Default)]
struct Typed {
    state: Option<PathBuf>,
    workspace: Option<String>,
    pool: Option<String>,
    custom: Option<String>,
    command_names: OnceLock<Vec<String>>,
}

/// Live values an argument can be completed with.
#[derive(Clone, Copy)]
enum Target {
    Commands,
    Repositories,
    Workspaces,
    Pools,
    Members,
    ResourceNames,
    AccessRequests,
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
    if !words.is_empty() {
        if let Ok(matches) = command
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
        }
    }
    for name in typed.command_names() {
        if command
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == name)
        {
            continue;
        }
        command = command.subcommand(
            Command::new(name)
                .about("Configured workspace command")
                .arg(clap::Arg::new("workspace"))
                .arg(clap::Arg::new("args").last(true).num_args(0..)),
        );
    }
    decorate(command, "", Arc::new(typed))
}

fn value(matches: &clap::ArgMatches, id: &str) -> Option<String> {
    matches.try_get_one::<String>(id).ok().flatten().cloned()
}

/// Attach live completers to the arguments that name daemon-managed objects.
fn decorate(command: Command, parent: &str, typed: Arc<Typed>) -> Command {
    let name = command.get_name().to_owned();
    command
        .mut_args(|arg| {
            let target = match (arg.get_id().as_str(), parent, name.as_str()) {
                ("id", "access", "approve" | "deny") => Some(Target::AccessRequests),
                ("name", _, "run") => Some(Target::Commands),
                ("repository", _, _) => Some(Target::Repositories),
                ("workspace", _, _) => Some(Target::Workspaces),
                ("pool", "resource", _) => Some(Target::Pools),
                ("resource", "resource", _) => Some(Target::Members),
                ("name", "resource", "release") => Some(Target::ResourceNames),
                ("name", "port", "acquire") => Some(Target::Ports),
                ("name", "port", "release") => Some(Target::ReservedPorts),
                ("name", "sim", "release") => Some(Target::SimNames),
                _ => None,
            };
            if let Some(target) = target {
                let typed = typed.clone();
                arg.add(ArgValueCompleter::new(move |current: &OsStr| {
                    typed.complete(target, current)
                }))
            } else if matches!(name.as_str(), "add" | "issue" | "review") && arg.get_id() == "agent"
            {
                let typed = typed.clone();
                arg.add(ArgValueCompleter::new(move |current: &OsStr| {
                    let mut names = crate::cli::Agent::possible_values();
                    names.extend(
                        typed.command_names().into_iter().filter(|name| {
                            matches!(name.parse(), Ok(crate::cli::Agent::Custom(_)))
                        }),
                    );
                    names
                        .into_iter()
                        .filter(|name| name.starts_with(current.to_string_lossy().as_ref()))
                        .map(CompletionCandidate::new)
                        .collect::<Vec<_>>()
                }))
            } else if parent == "skill" && name == "install" && arg.get_id() == "agent" {
                arg.add(ArgValueCompleter::new(|current: &OsStr| {
                    let mut names: std::collections::BTreeSet<_> = ["all"]
                        .into_iter()
                        .chain(crate::ai::BUILT_INS)
                        .map(str::to_owned)
                        .collect();
                    if let Some(home) = std::env::var_os("HOME") {
                        if let Ok(agents) = crate::ai::load(&PathBuf::from(home)) {
                            names.extend(agents.into_keys());
                        }
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
        .mut_subcommands(|subcommand| decorate(subcommand, &name, typed.clone()))
}

impl Typed {
    fn command_names(&self) -> Vec<String> {
        self.command_names
            .get_or_init(|| self.load_command_names())
            .clone()
    }

    fn load_command_names(&self) -> Vec<String> {
        let state = self
            .state
            .clone()
            .or_else(|| std::env::var_os(env::STATE_DIR).map(PathBuf::from));
        let Ok(paths) = Paths::new(state) else {
            return vec![];
        };
        let mut commands = crate::named_commands::defaults();
        if let Ok(config) = Config::load(&paths) {
            commands.extend(config.commands);
        }
        if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            let layer = runtime.block_on(async {
                tokio::time::timeout(Duration::from_millis(500), async {
                    let workspaces = client::workspaces(&paths).await?;
                    let workspace = self
                        .workspace(&workspaces)
                        .or_else(|| {
                            std::env::current_dir()
                                .ok()
                                .and_then(|cwd| Workspace::innermost(&workspaces, &cwd))
                        })
                        .context("no current workspace")?;
                    client::call(
                        &paths,
                        Method::LayeredConfig {
                            target: ConfigTarget::Workspace(workspace.id.clone()),
                        },
                    )
                    .await
                })
                .await
            });
            if let Ok(Ok(Body::LayeredConfig(layers))) = layer {
                commands.extend(layers.resolve().commands);
            }
        }
        if let Some(name) = &self.custom {
            commands.entry(name.clone()).or_default();
        }
        commands.into_keys().collect()
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
        if matches!(target, Target::AccessRequests) {
            if let Body::AccessRequests(requests) =
                client::call(&paths, Method::ListAccess { workspace: None }).await?
            {
                return Ok(requests
                    .into_iter()
                    .filter(|r| r.status == crate::access::Status::Pending)
                    .map(|r| CompletionCandidate::new(r.id))
                    .collect());
            }
            return Ok(Vec::new());
        }
        let workspaces = client::workspaces(&paths).await?;
        if matches!(target, Target::Workspaces) {
            return Ok(workspaces
                .into_iter()
                .map(|w| CompletionCandidate::new(w.name))
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
                if let Body::ResourceOverview(overview) =
                    client::call(&paths, Method::ResourceOverview { workspace }).await?
                {
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
            }
            Target::Ports | Target::ReservedPorts => {
                if let Body::PortOverview(overview) =
                    client::call(&paths, Method::PortOverview { workspace }).await?
                {
                    if matches!(target, Target::Ports) {
                        names.extend(overview.configured.into_keys());
                    }
                    names.extend(overview.reserved.into_iter().map(|p| p.name));
                }
            }
            Target::SimNames => {
                let method = Method::SimList {
                    workspace: Some(workspace),
                };
                if let Body::Simulators(simulators) = client::call(&paths, method).await? {
                    names.extend(simulators.into_iter().filter_map(|s| s.lease_name));
                }
            }
            Target::Commands
            | Target::Repositories
            | Target::Workspaces
            | Target::AccessRequests => unreachable!(),
        }
        Ok(names.into_iter().map(CompletionCandidate::new).collect())
    }

    /// The explicitly typed workspace, else the one containing the current
    /// directory, else the scoped execution's own workspace.
    fn workspace<'a>(&self, workspaces: &'a [Workspace]) -> Option<&'a Workspace> {
        if let Some(selector) = &self.workspace {
            return workspaces
                .iter()
                .find(|w| &w.name == selector || &w.id == selector);
        }
        std::env::current_dir()
            .ok()
            .and_then(|cwd| Workspace::innermost(workspaces, &cwd))
            .or_else(|| env::is_scoped().then(|| workspaces.first()).flatten())
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
