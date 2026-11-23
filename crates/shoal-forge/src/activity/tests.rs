use serde_json::json;

use super::*;
use shoal_forge_types::{
    ci::run_view::JobRef,
    items::{link::LinkKind, reference::ItemRef},
};

#[tokio::test]
async fn a_run_whose_jobs_were_cancelled_reports_it_and_keeps_their_logs() -> Result<()> {
    use crate::test_support::{Server, agent_on, repo_on};
    use shoal_forge_types::backends::ForgeKind;
    let server = Server::start(|request| {
        let body = match request.line.as_str() {
            "GET /api/v1/repos/team/repo/actions/runs?run_number=7" => json!({"workflow_runs": [
                {"id": 30, "index_in_repo": 7, "status": "cancelled", "prettyref": "main",
                    "workflow_id": "ci.yml", "commit_sha": "abc"}
            ]}),
            // `rust` was cancelled while it ran, `musl` before a runner took it.
            "GET /api/v1/repos/team/repo/actions/runs/30/jobs" => json!([
                {"id": 1, "name": "gate", "status": "success", "run_id": 30, "task_id": 5},
                {"id": 2, "name": "rust", "status": "cancelled", "run_id": 30, "task_id": 6},
                {"id": 3, "name": "musl", "status": "cancelled", "run_id": 30, "task_id": 0}
            ]),
            "GET /api/v1/repos/team/repo/actions/jobs/2/logs" => {
                return (
                    200,
                    Vec::new(),
                    "Compiling shoal\ncontext canceled\n".into(),
                );
            }
            _ => return (404, Vec::new(), Vec::new()),
        };
        (200, Vec::new(), body.to_string().into())
    });
    let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "agent");
    let run = ItemRef::numbered(&forgejo, LinkKind::Ci, 7)?;
    let activity = forgejo.run_activity(&account, &run, None).await?;
    assert_eq!(activity.state(), Some(SnapshotState::Cancelled));
    assert!(activity.failed_jobs.is_empty());
    assert!(
        !server
            .received()
            .iter()
            .any(|request| request.line.contains("/logs")),
        "a cancelled job is no failure to read the log of"
    );
    let log = forgejo
        .run_job_log(&account, &run, &JobRef::Name("rust".into()))
        .await?;
    assert!(log.text.contains("context canceled"));
    Ok(())
}

#[tokio::test]
async fn a_rerun_that_finishes_between_two_reads_reports_its_finish() -> Result<()> {
    use crate::test_support::{Server, agent_on, repo_on};
    use shoal_forge_types::{backends::ForgeKind, updates::UpdateKind};
    let read = |stopped: &'static str| async move {
        let server = Server::start(move |request| {
            if request.line != "GET /api/v1/repos/team/repo/actions/runs?run_number=7" {
                return (404, Vec::new(), Vec::new());
            }
            let body = json!({"workflow_runs": [
                {"id": 30, "index_in_repo": 7, "status": "success", "prettyref": "main",
                    "workflow_id": "ci.yml", "commit_sha": "abc", "stopped": stopped}
            ]});
            (200, Vec::new(), body.to_string().into())
        });
        let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
        let run = ItemRef::numbered(&forgejo, LinkKind::Ci, 7)?;
        forgejo
            .run_activity(&agent_on(&server, "agent"), &run, None)
            .await
    };
    let first = read("2026-11-21T23:08:00+01:00").await?;
    let url = "https://forge.example/team/repo/actions/runs/7";
    assert!(first.changes(&first, url).is_empty());
    let rerun = read("2026-11-21T23:28:04+01:00").await?;
    let updates = first.changes(&rerun, url);
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert_eq!(updates[0].kind, UpdateKind::CiCompleted);
    Ok(())
}

#[tokio::test]
async fn a_failed_run_read_again_at_the_same_finish_keeps_its_known_failure() -> Result<()> {
    use crate::test_support::{Server, agent_on, repo_on};
    use shoal_forge_types::backends::ForgeKind;
    let stopped = std::sync::Arc::new(std::sync::Mutex::new("23:08"));
    let server = Server::start({
        let stopped = stopped.clone();
        move |request| {
            let body = match request.line.as_str() {
                "GET /api/v1/repos/team/repo/actions/runs?run_number=7" => {
                    json!({"workflow_runs": [
                        {"id": 30, "index_in_repo": 7, "status": "failure", "prettyref": "main",
                            "workflow_id": "ci.yml", "commit_sha": "abc",
                            "stopped": *stopped.lock().unwrap()}
                    ]})
                }
                "GET /api/v1/repos/team/repo/actions/runs/30/jobs" => json!([
                    {"id": 2, "name": "rust", "status": "failure", "run_id": 30, "task_id": 6}
                ]),
                _ => return (404, Vec::new(), Vec::new()),
            };
            (200, Vec::new(), body.to_string().into())
        }
    });
    let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "agent");
    let run = ItemRef::numbered(&forgejo, LinkKind::Ci, 7)?;
    let job_reads = || {
        (server.received().iter())
            .filter(|request| request.line.contains("/jobs"))
            .count()
    };
    let first = forgejo.run_activity(&account, &run, None).await?;
    let read = job_reads();
    assert!(read > 0);
    let again = forgejo.run_activity(&account, &run, Some(&first)).await?;
    assert_eq!(job_reads(), read);
    assert_eq!(again.state(), Some(SnapshotState::Failed));
    assert_eq!(again.failed_jobs, first.failed_jobs);
    // A rerun's finish reads its own jobs.
    *stopped.lock().unwrap() = "23:28";
    forgejo.run_activity(&account, &run, Some(&first)).await?;
    assert!(job_reads() > read);
    Ok(())
}
