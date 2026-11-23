use serde_json::json;
use shoal_forge_types::backends::ForgeKind;

use super::*;
use crate::test_support::{Server, agent_on, repo_on};

fn runner() -> RegisteredRunner {
    RegisteredRunner {
        id: shoal_forge_types::numbers::RunnerId::new(5),
        uuid: "runner-uuid".into(),
        token: "runner-token".into(),
    }
}

/// A Forgejo whose job 8 runs as task `task`, and that answers an update of
/// a task with `result`.
fn forgejo(task: u64, result: &'static str) -> Server {
    Server::start(move |request| {
        let body = match request.line.as_str() {
            "GET /api/v1/repos/team/repo/actions/jobs/8" => json!({"id": 8, "task_id": task}),
            "POST /api/actions/runner.v1.RunnerService/UpdateTask" => {
                json!({"state": {"id": task.to_string(), "result": result}})
            }
            _ => return (404, Vec::new(), b"{}".to_vec()),
        };
        (200, Vec::new(), body.to_string().into())
    })
}

#[tokio::test]
async fn a_job_is_cancelled_as_the_runner_that_runs_it() {
    let server = forgejo(31, CANCELLED);
    let repo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "user");
    assert!(
        cancel_job(&repo, &account, &runner(), JobId::new(8))
            .await
            .unwrap()
    );
    let received = server.received();
    let update = (received.iter())
        .find(|request| request.line.ends_with("/UpdateTask"))
        .unwrap();
    assert_eq!(update.header("x-runner-uuid"), Some("runner-uuid"));
    assert_eq!(update.header("x-runner-token"), Some("runner-token"));
    assert_eq!(update.header("authorization"), None);
    let state: Value = serde_json::from_str(&update.body).unwrap();
    assert_eq!(state["state"]["id"], 31);
    assert_eq!(state["state"]["result"], CANCELLED);
    assert!(state["state"]["stoppedAt"].as_str().unwrap().ends_with('Z'));
}

#[tokio::test]
async fn a_task_that_finished_first_is_not_cancelled() {
    let server = forgejo(31, "RESULT_SUCCESS");
    let repo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "user");
    assert!(
        !cancel_job(&repo, &account, &runner(), JobId::new(8))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_job_no_runner_took_has_nothing_to_cancel() {
    let server = forgejo(0, CANCELLED);
    let repo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "user");
    (cancel_job(&repo, &account, &runner(), JobId::new(8)).await).unwrap_err();
    assert!(!(server.received().iter()).any(|request| request.line.ends_with("/UpdateTask")));
}

#[test]
fn credentials_that_would_leave_their_line_are_refused() {
    let quoted = RegisteredRunner {
        token: "a\"b".into(),
        ..runner()
    };
    credentials(&quoted).unwrap_err();
}
