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
        restarted_by: None,
        restart: Some(PendingRestart {
            run: Some("earlier".into()),
        }),
    };
    let mut restarts = vec![Restart {
        workspace,
        agent: agent.clone(),
        joins: true,
    }];
    // The restart ended before the run asked for it, and the agent works.
    let ended = RunningAgent {
        restarted_by: Some("earlier".into()),
        restart: None,
        ..agent
    };
    assert!(refresh(&mut restarts, &[ended]).is_empty());
    assert!(restarts[0].joins && restarts[0].ready());
}
