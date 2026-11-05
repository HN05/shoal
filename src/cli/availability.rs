//! What each CLI command needs of its workspace, and the facts the CLI
//! judges its workspaces against, which [`crate::client::availability`]
//! decides with.
use std::collections::{HashMap, HashSet};

pub(crate) use crate::client::availability::*;
use crate::{
    cli::terminal,
    model::{RunningAgent, Workspace},
    paths::Paths,
    sessions::Origin,
};

impl Need {
    /// The need of the built-in command at `path`, such as `["release", "hold"]`; `None` for other names, which run configured commands.
    pub fn of(path: &[&str]) -> Option<Self> {
        Some(match path {
            ["attach"] => Self::Session,
            ["cd"] => Self::Directory,
            ["stop" | "restart"] => Self::Running,
            ["repo", "labels"]
            | ["forge", "capabilities" | "reactions" | "api" | "download"]
            | ["ci", ..] => Self::Any,
            ["work", "setup"] => Self::Setup,
            ["work", "restore" | "rename"] | ["move", "export" | "ssh"] => Self::Any,
            ["work", "checkpoint"] => Self::Checkpoint,
            ["branch", "rename"] => Self::Settled,
            ["branch", "base", "clear"] => Self::Base,
            ["branch", "base", ..] => Self::Any,
            ["work", "env"] => Self::Ready,
            ["release", "hold"] => Self::Holds,
            ["link" | "unlink"] | ["issue", "open"] => Self::Links,
            [
                "run" | "edit" | "open" | "start" | "merge" | "unmerge" | "split" | "done"
                | "watch" | "ready" | "acquire" | "release",
                ..,
            ]
            | ["review", "run"]
            | ["pr", "open"]
            | ["questions", "ask"]
            | ["demo", "start"] => Self::Ready,
            [
                "status" | "diff" | "conflicts" | "rm" | "leases" | "messages" | "files"
                | "questions" | "approvals" | "view" | "resume" | "catalog" | "history" | "config"
                | "pr" | "issue" | "alerts" | "demo" | "queue" | "review",
                ..,
            ] => Self::Any,
            ["internal", ..] => Self::Any,
            _ => return None,
        })
    }

    /// The need of the command at `path`; configured commands need a ready
    /// workspace to run in.
    pub fn of_command(path: &[&str]) -> Self {
        Self::of(path).unwrap_or(Self::Ready)
    }

    /// What a picker reports when no workspace is left to offer.
    pub fn none_offered(self) -> &'static str {
        match self {
            Self::Session => "no workspace or chat has an agent session",
            Self::Running => "nothing is running in any workspace",
            Self::Done => "no workspace is done",
            Self::Base => "no workspace has a base workspace",
            Self::Holds => "no workspace has holds",
            Self::Marks => "no workspace is marked ready",
            Self::Directory => "no workspace directory exists",
            Self::Untracked => "no workspace or chat",
            Self::Any
            | Self::Ready
            | Self::Settled
            | Self::Links
            | Self::Setup
            | Self::Checkpoint => "no workspaces",
        }
    }

    /// Flags that change the operation also change which workspaces it offers.
    pub fn of_matches(path: &[&str], matches: &clap::ArgMatches) -> Option<Self> {
        let flag = |name| matches.try_get_one::<bool>(name).ok().flatten() == Some(&true);
        // Untracked processes do not need a running agent or command.
        if path == ["stop"] && (flag("processes") || flag("leftovers")) {
            return Some(Self::Untracked);
        }
        if flag("undo") {
            match path {
                ["done"] => return Some(Self::Done),
                ["ready"] => return Some(Self::Marks),
                _ => {}
            }
        }
        Self::of(path)
    }
}

/// The availability of `workspaces` for `need`, with the agent sessions the
/// terminal hosts keep.
pub async fn load(need: Need, paths: &Paths, workspaces: &[Workspace]) -> Availability {
    Availability::load(need, paths, workspaces, async || {
        session_workspaces(paths).await
    })
    .await
}

/// The IDs of workspaces with agent sessions, counting the places their
/// running agents were recorded in and those Shoal created for them, such as
/// a session left at its shell after a failed launch, but not the session
/// of an agent Shoal stopped to restore later.
async fn session_workspaces(paths: &Paths) -> HashSet<String> {
    let agents = crate::client::agents::all(paths).await.unwrap_or_default();
    let owned = crate::client::workspaces::owned_places(paths)
        .await
        .unwrap_or_default();
    let places = agent_places(&agents);
    let mut owners = terminal::session_owners(&places.connected);
    // Each host lists its sessions once for the owners still in question.
    let recorded: Vec<_> = places
        .unknown
        .into_iter()
        .chain(
            owned
                .iter()
                .map(|place| (place.workspace_id.as_str(), &place.origin)),
        )
        .filter(|(owner, _)| !owners.contains(*owner))
        .collect();
    let settings = workspace_settings(paths, recorded.iter().map(|(owner, _)| *owner)).await;
    owners.extend(terminal::open_session_owners(&recorded, &places.stopped, &settings).await);
    owners
}

/// Where agents were recorded running, as (workspace ID, place) pairs.
#[derive(Debug, Default, PartialEq, Eq)]
struct AgentPlaces<'a> {
    /// Connected agents, which run in their places.
    connected: Vec<(&'a str, &'a Origin)>,
    /// Connected agents Shoal stopped there to restore later.
    stopped: Vec<(&'a str, &'a Origin)>,
    /// Executions whose wrapper is gone, whose record outlives the session.
    unknown: Vec<(&'a str, &'a Origin)>,
}

/// Sort the places of `agents` by what runs in them; an agent without a
/// recorded place has none.
fn agent_places(agents: &[RunningAgent]) -> AgentPlaces<'_> {
    let mut places = AgentPlaces::default();
    for agent in agents {
        let Some(place) = agent.origin.as_ref() else {
            continue;
        };
        let kind = match (&agent.agent, &agent.stopped) {
            (None, _) => &mut places.unknown,
            (Some(_), Some(_)) => &mut places.stopped,
            (Some(_), None) => &mut places.connected,
        };
        kind.push((agent.space_id.as_str(), place));
    }
    places
}

/// The settings of each of `workspaces`, which say which hosts to ask about
/// its places; one whose settings cannot be read is left out.
async fn workspace_settings<'a>(
    paths: &Paths,
    workspaces: impl Iterator<Item = &'a str>,
) -> HashMap<String, crate::config::Effective> {
    let mut settings = HashMap::new();
    let Ok(config) = crate::config::Config::load_with_templates(paths) else {
        return settings;
    };
    for workspace in workspaces {
        if settings.contains_key(workspace) {
            continue;
        }
        let target = crate::protocol::ConfigTarget::Workspace(workspace.to_owned());
        let layers = crate::client::workspaces::config_layers(paths, target).await;
        if let Ok(resolved) = layers.and_then(|layers| config.resolve(&layers)) {
            settings.insert(workspace.to_owned(), resolved);
        }
    }
    settings
}

#[cfg(test)]
mod tests;
