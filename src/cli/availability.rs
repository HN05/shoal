//! What a command needs of the workspace it acts on, so completion and
//! pickers can say why a workspace would be refused instead of offering it
//! as if it would work.
use std::collections::{HashMap, HashSet};

use crate::{
    cli::{client, internal, list_status::ListStatus, terminal},
    model::Workspace,
    paths::Paths,
    state::WorkspaceState,
};

/// The condition a command's workspace must meet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Need {
    /// Any registered workspace, such as for reading its records.
    #[default]
    Any,
    /// A ready workspace; changes and executions require one.
    Ready,
    /// A ready workspace, or a failed one an interrupted operation left to finish.
    Settled,
    /// A workspace that accepts issue and PR link changes.
    Links,
    /// A workspace setup can reserve: preparing, ready or failed, with no
    /// execution running besides the caller's own.
    Setup,
    /// A workspace a checkpoint can be taken of: ready, stopping or failed.
    Checkpoint,
    /// A worktree directory to enter.
    Directory,
    /// An agent session to attach to.
    Session,
    /// A running agent or command to stop.
    Running,
    /// A ready workspace with a recorded completion to withdraw.
    Done,
    /// A recorded base workspace to clear.
    Base,
    /// A hold to release.
    Holds,
    /// A ready-for-review mark to withdraw.
    Marks,
}

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
                "run" | "edit" | "claude" | "codex" | "t3" | "happy" | "land" | "split" | "done"
                | "review" | "watch" | "ready" | "acquire" | "release",
                ..,
            ]
            | ["pr", "open"]
            | ["questions", "ask"]
            | ["demo", "start"] => Self::Ready,
            [
                "status" | "diff" | "conflicts" | "rm" | "leases" | "messages" | "files"
                | "questions" | "approvals" | "view" | "resume" | "catalog" | "history" | "config"
                | "pr" | "issue" | "alerts" | "demo" | "queue",
                ..,
            ] => Self::Any,
            [group, ..] if *group == internal::GROUP => Self::Any,
            _ => return None,
        })
    }

    /// Flags that change the operation also change which workspaces it offers.
    pub fn of_matches(path: &[&str], matches: &clap::ArgMatches) -> Option<Self> {
        if matches.try_get_one::<bool>("undo").ok().flatten() == Some(&true) {
            match path {
                ["done"] => return Some(Self::Done),
                ["ready"] => return Some(Self::Marks),
                _ => {}
            }
        }
        Self::of(path)
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
            Self::Any
            | Self::Ready
            | Self::Settled
            | Self::Links
            | Self::Setup
            | Self::Checkpoint => "no workspaces",
        }
    }
}

/// How a command's need judges one workspace.
#[derive(Debug, PartialEq, Eq)]
pub enum Fit {
    /// The command would act on it.
    Accepted,
    /// The command would refuse it for a reason worth showing, such as its
    /// state or a running execution.
    Refused(String),
    /// It lacks what the command acts on, such as an agent session to attach
    /// to; pickers and completion leave it out.
    Hidden,
}

/// A need with the live facts it is judged against.
pub struct Availability {
    need: Need,
    /// Workspace IDs with agent sessions, loaded only for [`Need::Session`].
    sessions: HashSet<String>,
    /// Workspace IDs with an execution that blocks setup, loaded only for
    /// [`Need::Setup`].
    busy: HashSet<String>,
}

impl Availability {
    pub async fn load(need: Need, paths: &Paths, workspaces: &[Workspace]) -> Self {
        let sessions = if need == Need::Session {
            session_workspaces(paths).await
        } else {
            HashSet::new()
        };
        let busy = if need == Need::Setup {
            busy_workspaces(paths, workspaces).await
        } else {
            HashSet::new()
        };
        Self {
            need,
            sessions,
            busy,
        }
    }

    /// How the command would treat `workspace`.
    pub fn fit(&self, workspace: &Workspace) -> Fit {
        let state = workspace.state;
        let accepts = |states: &[WorkspaceState]| states.contains(&state);
        let lacks = match self.need {
            Need::Directory => !workspace.path.is_dir(),
            Need::Session => !self.sessions.contains(&workspace.id),
            Need::Running => !workspace.running,
            Need::Done => !workspace.done,
            Need::Base => workspace.base_workspace.is_none(),
            Need::Holds => workspace.holds.is_empty(),
            Need::Marks => workspace.review.is_empty(),
            Need::Any
            | Need::Ready
            | Need::Settled
            | Need::Links
            | Need::Setup
            | Need::Checkpoint => false,
        };
        let settled = match self.need {
            Need::Ready | Need::Done => accepts(&[WorkspaceState::Ready]),
            Need::Settled => accepts(&[WorkspaceState::Ready, WorkspaceState::Failed]),
            Need::Links => state.accepts_links(),
            Need::Setup => accepts(&[
                WorkspaceState::Preparing,
                WorkspaceState::Ready,
                WorkspaceState::Failed,
            ]),
            Need::Checkpoint => accepts(&[
                WorkspaceState::Ready,
                WorkspaceState::Stopping,
                WorkspaceState::Failed,
            ]),
            Need::Any
            | Need::Directory
            | Need::Session
            | Need::Running
            | Need::Base
            | Need::Holds
            | Need::Marks => true,
        };
        if lacks {
            Fit::Hidden
        } else if !settled {
            // A workspace that is not ready is named by its state, as lists do.
            Fit::Refused(
                ListStatus::of_workspace(workspace, false)
                    .label()
                    .to_owned(),
            )
        } else if self.need == Need::Setup && self.busy.contains(&workspace.id) {
            Fit::Refused("running".to_owned())
        } else {
            Fit::Accepted
        }
    }
}

/// The IDs of workspaces with agent sessions, counting the places their
/// running agents were recorded in and those Shoal created for them, such as
/// a session left at its shell after a failed launch.
async fn session_workspaces(paths: &Paths) -> HashSet<String> {
    let agents: Vec<crate::model::RunningAgent> =
        client::request(paths, crate::protocol::ExecutionMethod::ListAgents)
            .await
            .unwrap_or_default();
    let owned: Vec<crate::model::OwnedPlace> =
        client::request(paths, crate::protocol::WorkspaceMethod::OwnedPlaces)
            .await
            .unwrap_or_default();
    // A connected agent runs in its place. The record of an execution whose
    // wrapper is gone, or of a place Shoal created, outlives its session.
    let (connected, unknown): (Vec<_>, Vec<_>) = agents
        .iter()
        .filter_map(|agent| Some((agent, agent.origin.as_ref()?)))
        .partition(|(agent, _)| agent.agent.is_some());
    let connected: Vec<_> = connected
        .into_iter()
        .map(|(agent, place)| (agent.workspace_id.as_str(), place))
        .collect();
    let mut owners = terminal::session_owners(&connected);
    // Each host lists its sessions once for the owners still in question.
    let recorded: Vec<_> = unknown
        .into_iter()
        .map(|(agent, place)| (agent.workspace_id.as_str(), place))
        .chain(
            owned
                .iter()
                .map(|place| (place.workspace_id.as_str(), &place.origin)),
        )
        .filter(|(owner, _)| !owners.contains(*owner))
        .collect();
    let settings = workspace_settings(paths, recorded.iter().map(|(owner, _)| *owner)).await;
    owners.extend(terminal::open_session_owners(&recorded, &settings).await);
    owners
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
        let layers = client::request::<Box<crate::config::repo::ConfigLayers>>(
            paths,
            crate::protocol::WorkspaceMethod::LayeredConfig {
                target: crate::protocol::ConfigTarget::Workspace(workspace.to_owned()),
            },
        )
        .await;
        if let Ok(resolved) = layers.and_then(|layers| config.resolve(&layers)) {
            settings.insert(workspace.to_owned(), resolved);
        }
    }
    settings
}

/// Workspaces whose running executions would block setup. Setup exempts
/// the caller's own execution, so the caller's workspace counts only with
/// another execution, as its inspection lists them.
async fn busy_workspaces(paths: &Paths, workspaces: &[Workspace]) -> HashSet<String> {
    let own = std::env::var(crate::env::EXECUTION_ID).ok();
    let mut busy = HashSet::new();
    for workspace in workspaces.iter().filter(|w| w.running) {
        let blocking = match &own {
            Some(own) => client::inspect(paths, workspace.id.clone())
                .await
                .map_or(true, |inspection| {
                    inspection.executions.iter().any(|e| &e.id != own)
                }),
            None => true,
        };
        if blocking {
            busy.insert(workspace.id.clone());
        }
    }
    busy
}

#[cfg(test)]
mod tests;
