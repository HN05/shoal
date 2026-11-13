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
    let number = workspace.number;
    let mut restarts = vec![Restart {
        space: PresentSpace::Workspace(workspace),
        agent: agent.clone(),
        joins: None,
    }];
    let stopped = RunningAgent {
        stopped: Some(crate::protocol::Protection {
            reason: "critical memory pressure".into(),
            resumes_when: "system load is healthy".into(),
        }),
        ..agent
    };
    let skipped = refresh(&mut restarts, &[stopped], "run");
    assert!(restarts.is_empty());
    let start = format!("shoal queue start {number}");
    assert!(skipped[0].reason.contains(&start), "{}", skipped[0].reason);
}

#[tokio::test]
async fn a_failed_restart_names_the_resume_and_keeps_the_daemons_error() {
    use crate::{
        paths::Paths,
        protocol::{self, Body, ErrorCode, Request, Response, SavedPiece},
        saved::{Saved, SavedCommand},
    };
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let paths = Paths::for_test(root.path());
    paths.prepare().unwrap();
    let listener = tokio::net::UnixListener::bind(&paths.socket).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: Request = protocol::read(&mut stream).await.unwrap();
        let saved = Saved {
            commands: vec![SavedCommand {
                id: "command".into(),
                path: "command.json".into(),
                argv: vec!["true".into()],
            }],
            ..Saved::default()
        };
        let piece = SavedPiece(serde_json::to_string(&saved).unwrap());
        for body in [Body::SavedPieces(vec![piece]), Body::Ok] {
            let response = Response::new(request.id, body);
            protocol::write(&mut stream, &response).await.unwrap();
        }
    });
    let workspace = Workspace::new_record(
        "repository".into(),
        "api".into(),
        "api".into(),
        Some("api".into()),
        WorkspaceState::Ready,
    );
    let refused = RemoteError {
        code: ErrorCode::ScopeDenied,
        message: "refused".into(),
    };
    let space = crate::model::PresentSpace::Workspace(workspace.clone());
    let error = with_restore(&paths, &space, refused.into()).await;
    server.await.unwrap();
    assert!(format!("{error:#}").contains(&format!("shoal restore {}", workspace.number)));
    let remote = error.downcast_ref::<RemoteError>().unwrap();
    assert_eq!(remote.code, ErrorCode::ScopeDenied);
}
