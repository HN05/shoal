//! What a command needs of the workspace it acts on, so completion and
//! pickers can say why a workspace would be refused instead of offering it
//! as if it would work.
use std::collections::HashSet;

use crate::{
    cli::{client, internal, list_status::ListStatus, zmx},
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
    /// The need of the built-in command at `path`, such as `["hold",
    /// "release"]`; `None` for other names, which run configured commands.
    pub fn of(path: &[&str]) -> Option<Self> {
        Some(match path {
            ["attach"] => Self::Session,
            ["cd"] => Self::Directory,
            ["stop"] => Self::Running,
            ["undone"] => Self::Done,
            ["unready"] => Self::Marks,
            ["setup"] => Self::Setup,
            ["checkpoint"] => Self::Checkpoint,
            ["base", "clear"] => Self::Base,
            ["hold", "release"] => Self::Holds,
            ["link" | "unlink"] | ["issue", "open"] => Self::Links,
            [
                "run" | "exec" | "edit" | "claude" | "codex" | "t3" | "happy" | "land" | "rename"
                | "done" | "env" | "review" | "ask" | "watch" | "ready" | "acquire" | "release",
                ..,
            ]
            | ["hold", "acquire"]
            | ["pr", "open"]
            | ["port" | "resource" | "sim", "acquire" | "release"] => Self::Ready,
            [
                "status" | "inspect" | "diff" | "conflicts" | "rm" | "doctor" | "leases"
                | "messages" | "questions" | "approvals" | "view" | "notify" | "message" | "resume"
                | "restore" | "base" | "hold" | "port" | "resource" | "sim" | "config" | "pr"
                | "issue" | "ci" | "labels",
                ..,
            ] => Self::Any,
            [group, ..] if *group == internal::GROUP => Self::Any,
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
            Self::Session => "no workspace has an agent session",
            Self::Running => "nothing is running in any workspace",
            Self::Done => "no workspace is done",
            Self::Base => "no workspace has a base workspace",
            Self::Holds => "no workspace has holds",
            Self::Marks => "no workspace is marked ready",
            Self::Directory => "no workspace directory exists",
            Self::Any | Self::Ready | Self::Links | Self::Setup | Self::Checkpoint => {
                "no workspaces"
            }
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
            zmx::session_workspaces().await.unwrap_or_default()
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
            Need::Any | Need::Ready | Need::Links | Need::Setup | Need::Checkpoint => false,
        };
        let settled = match self.need {
            Need::Ready | Need::Done => accepts(&[WorkspaceState::Ready]),
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
mod tests {
    use clap::CommandFactory;

    use super::*;

    /// Every built-in command path whose arguments name a workspace.
    fn workspace_commands(
        command: &clap::Command,
        path: &mut Vec<String>,
        found: &mut Vec<Vec<String>>,
    ) {
        if command
            .get_arguments()
            .any(|arg| arg.get_id() == "workspace")
        {
            found.push(path.clone());
        }
        for subcommand in command.get_subcommands() {
            path.push(subcommand.get_name().to_owned());
            workspace_commands(subcommand, path, found);
            path.pop();
        }
    }

    #[test]
    fn every_command_naming_a_workspace_states_its_need() {
        let mut found = vec![];
        workspace_commands(&crate::cli::Cli::command(), &mut vec![], &mut found);
        assert!(found.len() > 40, "{found:?}");
        for path in found {
            let path: Vec<_> = path.iter().map(String::as_str).collect();
            assert!(Need::of(&path).is_some(), "{path:?}");
        }
        // Opening a PR pushes through a tracked command; opening an issue
        // only links it.
        assert_eq!(Need::of(&["pr", "open"]), Some(Need::Ready));
        assert_eq!(Need::of(&["issue", "open"]), Some(Need::Links));
        assert_eq!(Need::of(&["review-tool"]), None);
        assert_eq!(Need::of_command(&["review-tool"]), Need::Ready);
    }

    fn workspace(state: WorkspaceState) -> Workspace {
        Workspace::new_record(
            "repository".into(),
            "work".into(),
            std::env::temp_dir(),
            "work".into(),
            state,
        )
    }

    fn fit(need: Need, workspace: &Workspace) -> Fit {
        Availability {
            need,
            sessions: HashSet::new(),
            busy: workspace
                .running
                .then(|| workspace.id.clone())
                .into_iter()
                .collect(),
        }
        .fit(workspace)
    }

    fn refused(reason: &str) -> Fit {
        Fit::Refused(reason.to_owned())
    }

    #[test]
    fn states_and_running_work_are_shown_and_missing_targets_hidden() {
        let ready = workspace(WorkspaceState::Ready);
        let failed = workspace(WorkspaceState::Failed);
        assert_eq!(fit(Need::Any, &failed), Fit::Accepted);
        assert_eq!(fit(Need::Ready, &ready), Fit::Accepted);
        assert_eq!(fit(Need::Ready, &failed), refused("failed"));
        assert_eq!(fit(Need::Links, &failed), Fit::Accepted);
        let removing = workspace(WorkspaceState::Removing);
        assert_eq!(fit(Need::Links, &removing), refused("removing"));
        assert_eq!(fit(Need::Checkpoint, &failed), Fit::Accepted);
        let preparing = workspace(WorkspaceState::Preparing);
        assert_eq!(fit(Need::Checkpoint, &preparing), refused("preparing"));
        assert_eq!(fit(Need::Setup, &failed), Fit::Accepted);
        assert_eq!(fit(Need::Setup, &preparing), Fit::Accepted);
        assert_eq!(fit(Need::Setup, &removing), refused("removing"));
        let mut running = ready.clone();
        running.running = true;
        assert_eq!(fit(Need::Setup, &running), refused("running"));
        // Nothing to act on hides the workspace, whatever its state.
        assert_eq!(fit(Need::Running, &running), Fit::Accepted);
        assert_eq!(fit(Need::Running, &ready), Fit::Hidden);
        assert_eq!(fit(Need::Done, &failed), Fit::Hidden);
        let mut done = failed.clone();
        done.done = true;
        assert_eq!(fit(Need::Done, &done), refused("failed"));
        assert_eq!(fit(Need::Holds, &ready), Fit::Hidden);
        assert_eq!(fit(Need::Base, &ready), Fit::Hidden);
        assert_eq!(fit(Need::Marks, &ready), Fit::Hidden);
        assert_eq!(fit(Need::Directory, &ready), Fit::Accepted);
        let mut moved = ready.clone();
        moved.path = std::env::temp_dir().join("shoal-availability-missing");
        assert_eq!(fit(Need::Directory, &moved), Fit::Hidden);
        assert_eq!(fit(Need::Session, &ready), Fit::Hidden);
        let attachable = Availability {
            need: Need::Session,
            sessions: HashSet::from([ready.id.clone()]),
            busy: HashSet::new(),
        };
        assert_eq!(attachable.fit(&ready), Fit::Accepted);
    }
}
