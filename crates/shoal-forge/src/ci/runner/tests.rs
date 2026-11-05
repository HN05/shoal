use serde_json::json;

use super::*;
use crate::{
    backends::ForgeKind,
    test_support::{Server, agent_on, repo_on},
};

fn lines(server: &Server) -> Vec<String> {
    server
        .received()
        .into_iter()
        .map(|request| request.line)
        .collect()
}

#[tokio::test]
async fn waiting_jobs_come_oldest_first_and_registration_round_trips() -> Result<()> {
    let server = Server::start(|request| {
        let body = match request.line.as_str() {
            "GET /api/v1/version" => json!({"version": "17.0.1+gitea-1.22.0"}),
            "GET /api/v1/repos/team/repo/actions/runners/jobs?labels=shoal" => json!([
                {"handle": "b", "id": 9, "run_id": 2, "name": "docs", "runs_on": ["shoal"], "status": "waiting"},
                {"handle": "a", "id": 4, "run_id": 1, "name": "rust", "runs_on": ["shoal"], "status": "waiting"},
            ]),
            "POST /api/v1/repos/team/repo/actions/runners" => {
                json!({"id": 5, "uuid": "u", "token": "t"})
            }
            "DELETE /api/v1/repos/team/repo/actions/runners/5" => {
                return (204, Vec::new(), Vec::new());
            }
            _ => return (404, Vec::new(), br#"{"message":"not found"}"#.to_vec()),
        };
        (200, Vec::new(), body.to_string().into())
    });
    let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "user");
    forgejo.require_runner(&account).await?;
    let jobs = forgejo.waiting_jobs(&account, &["shoal".into()]).await?;
    let handles: Vec<_> = jobs.iter().map(|job| job.handle.as_str()).collect();
    assert_eq!(handles, ["a", "b"]);
    let runner = forgejo.register_runner(&account, "shoal-host").await?;
    assert_eq!(runner.map(|runner| runner.uuid).as_deref(), Some("u"));
    forgejo.delete_runner(&account, 5).await?;
    // A runner deleted on the forge already counts as deleted.
    forgejo.delete_runner(&account, 6).await?;
    assert_eq!(
        lines(&server).last().map(String::as_str),
        Some("DELETE /api/v1/repos/team/repo/actions/runners/6")
    );
    Ok(())
}

#[tokio::test]
async fn gitea_and_older_forgejo_run_no_jobs_and_other_forges_refuse_before_asking() -> Result<()> {
    let server = Server::start(|_| {
        let body = json!({"version": "1.22.0"});
        (200, Vec::new(), body.to_string().into())
    });
    let gitea = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "user");
    assert!(gitea.require_runner(&account).await.is_err());

    let gitlab = repo_on(&server, ForgeKind::GitLab, "team/repo");
    let received = server.received().len();
    assert!(gitlab.runner_program().is_err());
    assert!(gitlab.waiting_jobs(&account, &[]).await.is_err());
    assert_eq!(server.received().len(), received);
    Ok(())
}
