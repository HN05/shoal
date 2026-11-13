//! What each CLI command needs of its workspace, so completion and pickers
//! can say why a workspace would be refused instead of offering it as if it
//! would work, and the facts the CLI judges its workspaces against.
use std::collections::{HashMap, HashSet};

use crate::client::{execution_ids::ExecutionId, space_ids::SpaceId, space_names::SpaceSelector};
use crate::{
    cli::{list_status::ListStatus, terminal},
    client::connection::Daemon,
    model::{RunningAgent, Workspace},
    paths::CommandPaths,
    sessions::Origin,
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
    /// A space whose untracked processes to stop: a settled workspace, or a
    /// running chat.
    Untracked,
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
    /// The need of the command at `path`, such as `["release", "hold"]`;
    /// `None` for a command that names no workspace.
    pub fn of(path: &[&str]) -> Option<Self> {
        Some(match path {
            ["attach"] => Self::Session,
            ["cd"] => Self::Directory,
            ["stop" | "restart"] => Self::Running,
            ["repo", "labels"]
            | ["forge", "capabilities" | "reactions" | "api" | "download"]
            | ["ci", ..] => Self::Any,
            ["setup"] => Self::Setup,
            ["status" | "restore" | "rename"] | ["move", "export" | "ssh"] => Self::Any,
            ["convert", ..] => Self::Any,
            ["checkpoint"] => Self::Checkpoint,
            ["branch", "rename"] => Self::Settled,
            ["branch", "base", "clear"] => Self::Base,
            ["branch", "base", ..] => Self::Any,
            ["internal", "env"] => Self::Ready,
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
                "diff" | "conflicts" | "rm" | "leases" | "messages" | "files" | "questions"
                | "approvals" | "view" | "resume" | "catalog" | "history" | "config" | "pr"
                | "issue" | "alerts" | "demo" | "queue" | "review",
                ..,
            ] => Self::Any,
            ["internal", ..] => Self::Any,
            _ => return None,
        })
    }

    /// The need of the command at `path`, a ready workspace where
    /// [`Self::of`] states none.
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
    pub need: Need,
    /// Workspace IDs with agent sessions, loaded only for [`Need::Session`].
    pub sessions: HashSet<SpaceId>,
    /// Workspace IDs with an execution that blocks setup, loaded only for
    /// [`Need::Setup`].
    pub busy: HashSet<SpaceId>,
}

impl Availability {
    /// The facts `need` is judged against: for [`Need::Session`], the IDs
    /// of the workspaces with agent sessions, which `sessions` finds where
    /// the caller's terminal hosts keep them.
    pub async fn load(
        need: Need,
        daemon: &impl Daemon,
        workspaces: &[Workspace],
        sessions: impl AsyncFnOnce() -> HashSet<SpaceId>,
    ) -> Self {
        let sessions = if need == Need::Session {
            sessions().await
        } else {
            HashSet::new()
        };
        let busy = if need == Need::Setup {
            busy_workspaces(daemon, workspaces).await
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
            | Need::Untracked
            | Need::Links
            | Need::Setup
            | Need::Checkpoint => false,
        };
        let settled = match self.need {
            Need::Ready | Need::Done => accepts(&[WorkspaceState::Ready]),
            Need::Settled | Need::Untracked => {
                accepts(&[WorkspaceState::Ready, WorkspaceState::Failed])
            }
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

/// Workspaces whose running executions would block setup. Setup exempts
/// the caller's own execution, so the caller's workspace counts only with
/// another execution, as its inspection lists them.
async fn busy_workspaces(daemon: &impl Daemon, workspaces: &[Workspace]) -> HashSet<SpaceId> {
    let own = std::env::var(crate::client::env::EXECUTION_ID)
        .ok()
        .map(ExecutionId::from);
    let mut busy = HashSet::new();
    for workspace in workspaces.iter().filter(|w| w.running) {
        let blocking = match &own {
            Some(own) => {
                crate::client::workspaces::inspect(daemon, SpaceSelector::id(&workspace.id))
                    .await
                    .map_or(true, |inspection| {
                        inspection.executions.iter().any(|e| &e.id != own)
                    })
            }
            None => true,
        };
        if blocking {
            busy.insert(workspace.id.clone());
        }
    }
    busy
}

/// The availability of `workspaces` for `need`, with the agent sessions the
/// terminal hosts keep.
pub async fn load(need: Need, paths: &CommandPaths, workspaces: &[Workspace]) -> Availability {
    Availability::load(need, paths, workspaces, async || {
        session_workspaces(paths).await
    })
    .await
}

/// The IDs of workspaces with agent sessions, counting the places their
/// connected agents were recorded in, including one Shoal stopped whose
/// wrapper waits to restore it, which attaching resumes, and those Shoal
/// created for them, such as a session left at its shell after a failed
/// launch.
async fn session_workspaces(paths: &CommandPaths) -> HashSet<SpaceId> {
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
        .chain(owned.iter().map(|place| (&place.space_id, &place.origin)))
        .filter(|(owner, _)| !owners.contains(*owner))
        .collect();
    let settings = workspace_settings(paths, recorded.iter().map(|(owner, _)| *owner)).await;
    owners.extend(terminal::open_session_owners(&recorded, &settings).await);
    owners
}

/// Where agents were recorded running, as (workspace ID, place) pairs.
#[derive(Debug, Default, PartialEq, Eq)]
struct AgentPlaces<'a> {
    /// Connected agents, which run in their places or, stopped by Shoal,
    /// wait there to be restored.
    connected: Vec<(&'a SpaceId, &'a Origin)>,
    /// Executions whose wrapper is gone, whose record outlives the session.
    unknown: Vec<(&'a SpaceId, &'a Origin)>,
}

/// Sort the places of `agents` by what runs in them; an agent without a
/// recorded place has none.
fn agent_places(agents: &[RunningAgent]) -> AgentPlaces<'_> {
    let mut places = AgentPlaces::default();
    for agent in agents {
        let Some(place) = agent.origin.as_ref() else {
            continue;
        };
        let kind = match &agent.agent {
            None => &mut places.unknown,
            Some(_) => &mut places.connected,
        };
        kind.push((&agent.space_id, place));
    }
    places
}

/// The settings of each of `workspaces`, which say which hosts to ask about
/// its places; one whose settings cannot be read is left out.
async fn workspace_settings<'a>(
    paths: &CommandPaths,
    workspaces: impl Iterator<Item = &'a SpaceId>,
) -> HashMap<SpaceId, crate::config::Effective> {
    let mut settings = HashMap::new();
    let Ok(config) = crate::config::Config::load_with_templates(paths) else {
        return settings;
    };
    for workspace in workspaces {
        if settings.contains_key(workspace) {
            continue;
        }
        let target = crate::protocol::ConfigTarget::Workspace(SpaceSelector::id(workspace));
        let layers = crate::client::workspaces::config_layers(paths, target).await;
        if let Ok(resolved) = layers.and_then(|layers| config.resolve(&layers)) {
            settings.insert(workspace.clone(), resolved);
        }
    }
    settings
}

#[cfg(test)]
mod tests;
