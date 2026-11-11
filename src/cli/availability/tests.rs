use clap::CommandFactory;

use super::*;
use crate::state::WorkspaceState;

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
    let mut command = crate::cli::Cli::command();
    command.build();
    workspace_commands(&command, &mut vec![], &mut found);
    assert!(found.len() > 40, "{found:?}");
    for path in found {
        let path: Vec<_> = path.iter().map(String::as_str).collect();
        assert!(Need::of(&path).is_some(), "{path:?}");
    }
}

fn workspace(state: WorkspaceState) -> Workspace {
    Workspace::new_record(
        "repository".into(),
        "work".into(),
        std::env::temp_dir(),
        Some("work".into()),
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

#[test]
fn agent_places_set_stopped_agents_apart() {
    let place =
        |name: &str| crate::sessions::Origin::created(crate::sessions::HostName::Zmx, name.into());
    let (running, stopped, gone) = (place("running"), place("stopped"), place("gone"));
    let agent = |workspace: &str, origin: Option<&Origin>, name: Option<&str>| RunningAgent {
        execution: format!("{workspace}-execution").into(),
        space_id: workspace.into(),
        agent: name.map(Into::into),
        restorable: true,
        state: None,
        interrupted: false,
        origin: origin.cloned(),
        restarted_by: Vec::new(),
        restart: None,
        stopped: None,
        restarted_from: Vec::new(),
    };
    let agents = [
        agent("w1", Some(&running), Some("claude")),
        RunningAgent {
            stopped: Some(crate::protocol::Protection {
                reason: "critical memory pressure".into(),
                resumes_when: "an update arrives for it".into(),
            }),
            ..agent("w2", Some(&stopped), Some("claude"))
        },
        agent("w3", Some(&gone), None),
        agent("w4", None, Some("codex")),
    ];
    let [w1, w2, w3] = ["w1", "w2", "w3"].map(SpaceId::from);
    assert_eq!(
        agent_places(&agents),
        AgentPlaces {
            connected: vec![(&w1, &running)],
            stopped: vec![(&w2, &stopped)],
            unknown: vec![(&w3, &gone)],
        }
    );
}
