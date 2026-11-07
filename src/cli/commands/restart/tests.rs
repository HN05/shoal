use super::*;
use crate::{
    model::{PendingRestart, PresentSpace, Workspace},
    state::{AgentState, WorkspaceState},
};

#[test]
fn a_joined_restart_stays_joined_once_it_ends() {
    let workspace = Workspace::new_record(
        "repository".into(),
        "api".into(),
        "api".into(),
        Some("api".into()),
        WorkspaceState::Ready,
    );
    let agent = RunningAgent {
        execution: "api-execution".into(),
        space_id: workspace.id.clone(),
        agent: Some("claude".into()),
        restorable: true,
        state: Some(AgentState::Working),
        interrupted: false,
        origin: None,
        restarted_by: Vec::new(),
        restart: Some(PendingRestart {
            run: Some("earlier".into()),
            joined: Vec::new(),
        }),
        stopped: None,
        restarted_from: Vec::new(),
    };
    let mut restarts = vec![Restart {
        space: PresentSpace::Workspace(workspace),
        agent: agent.clone(),
        joins: agent.restart.clone(),
    }];
    // The restart ended before the run asked for it, and the agent works.
    let ended = RunningAgent {
        restarted_by: vec!["earlier".into()],
        restart: None,
        ..agent
    };
    assert!(refresh(&mut restarts, std::slice::from_ref(&ended), "earlier").is_empty());
    assert!(restarts[0].joins.is_some() && restarts[0].ready());
    // A restart in a new wrapper started the agent as another execution.
    let renewed = RunningAgent {
        execution: "api-renewed".into(),
        restarted_from: vec!["api-execution".into()],
        ..ended
    };
    assert!(refresh(&mut restarts, &[renewed], "earlier").is_empty());
    assert_eq!(restarts[0].agent.execution, "api-renewed");
}

#[test]
fn an_agent_another_command_of_the_run_restarted_is_skipped() {
    let workspace = Workspace::new_record(
        "repository".into(),
        "web".into(),
        "web".into(),
        Some("web".into()),
        WorkspaceState::Ready,
    );
    let agent = RunningAgent {
        execution: "web-execution".into(),
        space_id: workspace.id.clone(),
        agent: Some("claude".into()),
        restorable: true,
        state: Some(AgentState::Working),
        interrupted: false,
        origin: None,
        restarted_by: Vec::new(),
        restart: None,
        stopped: None,
        restarted_from: Vec::new(),
    };
    let mut restarts = vec![Restart {
        space: PresentSpace::Workspace(workspace),
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
    assert_eq!(skipped.len(), 1);
}

#[test]
fn an_agent_protection_stopped_while_the_run_waited_is_skipped() {
    let workspace = Workspace::new_record(
        "repository".into(),
        "docs".into(),
        "docs".into(),
        Some("docs".into()),
        WorkspaceState::Ready,
    );
    let agent = RunningAgent {
        execution: "docs-execution".into(),
        space_id: workspace.id.clone(),
        agent: Some("claude".into()),
        restorable: true,
        state: Some(AgentState::Working),
        interrupted: false,
        origin: None,
        restarted_by: Vec::new(),
        restart: None,
        stopped: None,
        restarted_from: Vec::new(),
    };
    let mut restarts = vec![Restart {
        space: PresentSpace::Workspace(workspace),
        agent: agent.clone(),
        joins: None,
    }];
    let stopped = RunningAgent {
        stopped: Some(crate::client::protocol::Protection {
            reason: "critical memory pressure".into(),
            resumes_when: "system load is healthy".into(),
        }),
        ..agent
    };
    let skipped = refresh(&mut restarts, &[stopped], "run");
    assert!(restarts.is_empty());
    assert!(skipped[0].reason.contains("shoal resume docs"));
}
