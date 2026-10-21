//! What a command needs of the workspace it acts on, so completion and
//! pickers can say why a workspace would be refused instead of offering it
//! as if it would work.
use std::collections::HashSet;

use crate::{
    cli::{internal, list_status::ListStatus, zmx},
    model::Workspace,
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
    /// A ready or failed workspace with nothing running, for setup.
    Setup,
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
            ["base", "clear"] => Self::Base,
            ["hold", "release"] => Self::Holds,
            ["link" | "unlink"] | ["pr" | "issue", "open"] => Self::Links,
            [
                "run" | "exec" | "edit" | "claude" | "codex" | "t3" | "happy" | "land" | "rename"
                | "done" | "env" | "review" | "ask" | "watch" | "ready" | "acquire" | "release",
                ..,
            ]
            | ["hold", "acquire"]
            | ["port" | "resource" | "sim", "acquire" | "release"] => Self::Ready,
            [
                "status" | "inspect" | "diff" | "conflicts" | "rm" | "doctor" | "leases"
                | "messages" | "questions" | "approvals" | "view" | "notify" | "message" | "resume"
                | "restore" | "base" | "hold" | "port" | "resource" | "sim" | "config" | "pr"
                | "issue" | "ci",
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
}

/// A need with the live facts it is judged against.
pub struct Availability {
    need: Need,
    /// Workspace IDs with agent sessions, loaded only for [`Need::Session`].
    sessions: HashSet<String>,
}

impl Availability {
    pub async fn load(need: Need) -> Self {
        let sessions = if need == Need::Session {
            zmx::session_workspaces().await.unwrap_or_default()
        } else {
            HashSet::new()
        };
        Self { need, sessions }
    }

    /// Why the command would refuse `workspace`, as pickers and completion
    /// describe it; `None` when it would act on it.
    pub fn refusal(&self, workspace: &Workspace) -> Option<String> {
        let state = workspace.state;
        // A workspace that is not ready is named by its state, as lists do.
        let unsettled = || {
            Some(
                ListStatus::of_workspace(workspace, false)
                    .label()
                    .to_owned(),
            )
        };
        match self.need {
            Need::Any => None,
            Need::Ready | Need::Done if state != WorkspaceState::Ready => unsettled(),
            Need::Ready => None,
            Need::Links if !state.accepts_links() => unsettled(),
            Need::Links => None,
            Need::Setup if !matches!(state, WorkspaceState::Ready | WorkspaceState::Failed) => {
                unsettled()
            }
            Need::Setup => workspace.running.then(|| "running".to_owned()),
            Need::Directory => (!workspace.path.is_dir()).then(|| "directory missing".to_owned()),
            Need::Session => {
                (!self.sessions.contains(&workspace.id)).then(|| "no agent session".to_owned())
            }
            Need::Running => (!workspace.running).then(|| "nothing running".to_owned()),
            Need::Done => (!workspace.done).then(|| "not done".to_owned()),
            Need::Base => workspace
                .base_workspace
                .is_none()
                .then(|| "no base workspace".to_owned()),
            Need::Holds => workspace.holds.is_empty().then(|| "no holds".to_owned()),
            Need::Marks => workspace
                .review
                .is_empty()
                .then(|| "not marked ready".to_owned()),
        }
    }
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

    fn refusal(need: Need, workspace: &Workspace) -> Option<String> {
        Availability {
            need,
            sessions: HashSet::new(),
        }
        .refusal(workspace)
    }

    #[test]
    fn refusals_name_the_missing_condition() {
        let ready = workspace(WorkspaceState::Ready);
        let failed = workspace(WorkspaceState::Failed);
        assert_eq!(refusal(Need::Any, &failed), None);
        assert_eq!(refusal(Need::Ready, &ready), None);
        assert_eq!(refusal(Need::Ready, &failed).as_deref(), Some("failed"));
        assert_eq!(refusal(Need::Links, &failed), None);
        assert_eq!(
            refusal(Need::Links, &workspace(WorkspaceState::Removing)).as_deref(),
            Some("removing")
        );
        assert_eq!(refusal(Need::Setup, &failed), None);
        assert_eq!(
            refusal(Need::Setup, &workspace(WorkspaceState::Preparing)).as_deref(),
            Some("preparing")
        );
        let mut running = ready.clone();
        running.running = true;
        assert_eq!(refusal(Need::Setup, &running).as_deref(), Some("running"));
        assert_eq!(refusal(Need::Running, &running), None);
        assert_eq!(
            refusal(Need::Running, &ready).as_deref(),
            Some("nothing running")
        );
        assert_eq!(refusal(Need::Done, &ready).as_deref(), Some("not done"));
        let mut done = failed.clone();
        done.done = true;
        assert_eq!(refusal(Need::Done, &done).as_deref(), Some("failed"));
        assert_eq!(refusal(Need::Directory, &ready), None);
        let mut moved = ready.clone();
        moved.path = std::env::temp_dir().join("shoal-availability-missing");
        assert_eq!(
            refusal(Need::Directory, &moved).as_deref(),
            Some("directory missing")
        );
        assert_eq!(
            refusal(Need::Session, &ready).as_deref(),
            Some("no agent session")
        );
        let attachable = Availability {
            need: Need::Session,
            sessions: HashSet::from([ready.id.clone()]),
        };
        assert_eq!(attachable.refusal(&ready), None);
    }
}
