use super::*;
use crate::{
    model::{PendingRestart, Workspace},
    state::{AgentState, WorkspaceState},
};

#[test]
fn a_joined_restart_stays_joined_once_it_ends() {
    let workspace = Workspace::new_record(
        "repository".into(),
        "api".into(),
        "api".into(),
        "api".into(),
        WorkspaceState::Ready,
    );
    let agent = RunningAgent {
        execution: "api-execution".into(),
        workspace_id: workspace.id.clone(),
        agent: Some("claude".into()),
        restorable: true,
        state: Some(AgentState::Working),
        interrupted: false,
        origin: None,
        restarted_by: Vec::new(),
        restart: Some(PendingRestart {
            run: Some("earlier".into()),
        }),
    };
    let mut restarts = vec![Restart {
        workspace,
        agent: agent.clone(),
        joins: agent.restart.clone(),
    }];
    // The restart ended before the run asked for it, and the agent works.
    let ended = RunningAgent {
        restarted_by: vec!["earlier".into()],
        restart: None,
        ..agent
    };
    assert!(refresh(&mut restarts, &[ended], "earlier").is_empty());
    assert!(restarts[0].joins.is_some() && restarts[0].ready());
}

#[test]
fn an_agent_another_command_of_the_run_restarted_is_skipped() {
    let workspace = Workspace::new_record(
        "repository".into(),
        "web".into(),
        "web".into(),
        "web".into(),
        WorkspaceState::Ready,
    );
    let agent = RunningAgent {
        execution: "web-execution".into(),
        workspace_id: workspace.id.clone(),
        agent: Some("claude".into()),
        restorable: true,
        state: Some(AgentState::Working),
        interrupted: false,
        origin: None,
        restarted_by: Vec::new(),
        restart: None,
    };
    let mut restarts = vec![Restart {
        workspace,
        agent: agent.clone(),
        joins: None,
    }];
    let restarted = RunningAgent {
        restarted_by: vec!["run".into()],
        ..agent
    };
    // Another run's restart does not count.
    assert!(refresh(&mut restarts, std::slice::from_ref(&restarted), "other").is_empty());
    let skipped = refresh(&mut restarts, &[restarted], "run");
    assert!(restarts.is_empty());
    assert_eq!(
        skipped[0].reason,
        "another command of this run restarted it already"
    );
}
