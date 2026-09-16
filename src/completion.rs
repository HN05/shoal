//! Completion runs before the normal CLI: read-only, scoped, and time bounded.
use std::{ffi::OsStr, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use clap::{Command, CommandFactory};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};

use crate::{
    client,
    model::Workspace,
    paths::Paths,
    protocol::{Body, Method},
};

pub const ENV: &str = "SHOAL_COMPLETE";

#[derive(Default)]
struct Context {
    state: Option<PathBuf>,
    workspace: Option<String>,
    pool: Option<String>,
}

#[derive(Clone, Copy)]
enum Kind {
    Repositories,
    Workspaces,
    Pools,
    Members,
    ResourceNames,
    Ports,
    ReservedPorts,
    SimNames,
}

pub fn command() -> Command {
    let command = crate::cli::Cli::command();
    let words: Vec<_> = std::env::args_os()
        .skip_while(|s| s != "--")
        .skip(1)
        .collect();
    let mut context = Context::default();
    if !words.is_empty() {
        if let Ok(matches) = command
            .clone()
            .ignore_errors(true)
            .try_get_matches_from(words)
        {
            context.state = matches
                .try_get_one::<PathBuf>("state_dir")
                .ok()
                .flatten()
                .cloned();
            let mut leaf = &matches;
            while let Some((_, child)) = leaf.subcommand() {
                leaf = child;
            }
            context.workspace = value(leaf, "workspace");
            context.pool = value(leaf, "pool");
        }
    }
    decorate(command, "", Arc::new(context))
}

fn value(matches: &clap::ArgMatches, id: &str) -> Option<String> {
    matches.try_get_one::<String>(id).ok().flatten().cloned()
}

fn decorate(command: Command, parent: &str, context: Arc<Context>) -> Command {
    let name = command.get_name().to_owned();
    command
        .mut_args(|arg| {
            let kind = match (arg.get_id().as_str(), parent, name.as_str()) {
                ("repository", _, _) => Some(Kind::Repositories),
                ("workspace", _, _) => Some(Kind::Workspaces),
                ("pool", "resource", _) => Some(Kind::Pools),
                ("resource", "resource", _) => Some(Kind::Members),
                ("name", "resource", "release") => Some(Kind::ResourceNames),
                ("name", "port", "reserve") => Some(Kind::Ports),
                ("name", "port", "release") => Some(Kind::ReservedPorts),
                ("name", "sim", "release") => Some(Kind::SimNames),
                _ => None,
            };
            if let Some(kind) = kind {
                let context = context.clone();
                arg.add(ArgValueCompleter::new(move |current: &OsStr| {
                    context.complete(kind, current)
                }))
            } else if parent == "repo" && name == "add" && arg.get_id() == "source" {
                arg.value_hint(clap::ValueHint::DirPath)
            } else {
                arg
            }
        })
        .mut_subcommands(|subcommand| decorate(subcommand, &name, context.clone()))
}

impl Context {
    fn complete(&self, kind: Kind, current: &OsStr) -> Vec<CompletionCandidate> {
        let Some(current) = current.to_str() else {
            return vec![];
        };
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return vec![];
        };
        let values = runtime.block_on(async {
            tokio::time::timeout(Duration::from_millis(500), self.candidates(kind, current)).await
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

    async fn candidates(&self, kind: Kind, current: &str) -> Result<Vec<CompletionCandidate>> {
        let state = self
            .state
            .clone()
            .or_else(|| std::env::var_os("SHOAL_STATE_DIR").map(PathBuf::from));
        let paths = Paths::new(state)?;
        if matches!(kind, Kind::Repositories) {
            let Body::Repositories(repos) = client::call(&paths, Method::Repositories).await?
            else {
                return Ok(vec![]);
            };
            let mut candidates = vec![];
            for repo in &repos {
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
                candidates
                    .push(CompletionCandidate::new(target).help(Some(repo.source.clone().into())));
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
            return Ok(candidates);
        }
        let Body::Workspaces(workspaces) = client::call(&paths, Method::List).await? else {
            return Ok(vec![]);
        };
        if matches!(kind, Kind::Workspaces) {
            return Ok(workspaces
                .into_iter()
                .map(|w| CompletionCandidate::new(w.name))
                .collect());
        }
        let workspace = self
            .workspace(&workspaces)
            .context("no current workspace")?;
        let target = workspace.id.clone();
        let mut names = vec![];
        match kind {
            Kind::Pools | Kind::Members | Kind::ResourceNames => {
                if let Body::ResourceOverview(overview) =
                    client::call(&paths, Method::ResourceOverview { workspace: target }).await?
                {
                    match kind {
                        Kind::Pools => names.extend(overview.pools.into_iter().map(|p| p.name)),
                        Kind::Members => names.extend(
                            overview
                                .pools
                                .into_iter()
                                .filter(|p| Some(&p.name) == self.pool.as_ref())
                                .flat_map(|p| p.resources.into_iter().map(|r| r.name)),
                        ),
                        Kind::ResourceNames => names.extend(
                            overview
                                .leases
                                .into_iter()
                                .filter(|l| Some(&l.pool) == self.pool.as_ref())
                                .map(|l| l.name),
                        ),
                        _ => unreachable!(),
                    }
                }
            }
            Kind::Ports | Kind::ReservedPorts => {
                if let Body::PortOverview(overview) =
                    client::call(&paths, Method::PortOverview { workspace: target }).await?
                {
                    if matches!(kind, Kind::Ports) {
                        names.extend(overview.configured.into_keys());
                    }
                    names.extend(overview.reserved.into_iter().map(|p| p.name));
                }
            }
            Kind::SimNames => {
                if let Body::Simulators(simulators) = client::call(
                    &paths,
                    Method::SimList {
                        workspace: Some(target),
                    },
                )
                .await?
                {
                    names.extend(simulators.into_iter().filter_map(|s| s.lease_name));
                }
            }
            _ => unreachable!(),
        }
        Ok(names.into_iter().map(CompletionCandidate::new).collect())
    }

    fn workspace<'a>(&self, workspaces: &'a [Workspace]) -> Option<&'a Workspace> {
        if let Some(selector) = &self.workspace {
            return workspaces
                .iter()
                .find(|w| &w.name == selector || &w.id == selector);
        }
        let current = std::env::current_dir().ok();
        workspaces
            .iter()
            .filter(|w| {
                current.as_ref().is_some_and(|cwd| {
                    std::fs::canonicalize(&w.path).is_ok_and(|path| cwd.starts_with(path))
                })
            })
            .max_by_key(|w| w.path.components().count())
            .or_else(|| std::env::var_os("SHOAL_SCOPE_TOKEN").and_then(|_| workspaces.first()))
    }
}
