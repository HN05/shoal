use serde_json::json;

use super::*;
use crate::{
    daemon::workspace::runner::tests::{admit_and_claim, github_fixture, github_job, launched},
    test_support::git,
};

/// Claim GitHub's job 11 of run 1 and report its runner took a job of `run`,
/// returning the claim and the job's directory.
async fn report(manager: &Manager, run: u64) -> (ClaimedJob, std::path::PathBuf) {
    admit_and_claim(manager).await;
    let job = manager.claimed_jobs().await.unwrap().remove(0);
    launched(&job.workspace_id);
    let directory = manager.job_directory(&job.id);
    std::fs::write(directory.join("took-run"), format!("{run}\n")).unwrap();
    (job, directory)
}

fn taken(id: u64, run: u64, name: &str) -> serde_json::Value {
    json!({"id": id, "run_id": run, "name": name, "status": "in_progress", "labels": ["shoal"], "runner_id": 77})
}

#[tokio::test]
async fn a_job_of_the_claimed_run_goes_on_and_its_claim_names_it() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    {
        let mut state = fixture.state.lock().unwrap();
        state.queued = vec![github_job(11, 1)];
        state.taken.insert(1, taken(12, 1, "docs"));
    }
    let (job, directory) = report(manager, 1).await;
    manager.follow_taken_jobs().await.unwrap();
    assert!(directory.join("prepared").exists());
    let claimed = manager.claimed_jobs().await.unwrap().remove(0);
    assert_eq!(
        (claimed.job.job.id, claimed.job.job.name.as_str()),
        (12, "docs")
    );
    let workspace = manager.workspace(&job.workspace_id).await.unwrap();
    assert_eq!(
        git(&workspace.path, &["rev-parse", "HEAD"]).trim(),
        fixture.head
    );
}

#[tokio::test]
async fn a_job_of_another_run_waits_until_its_workspace_is_at_that_runs_commit() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    {
        let mut state = fixture.state.lock().unwrap();
        state.queued = vec![github_job(11, 1)];
        state.taken.insert(2, taken(21, 2, "rust"));
    }
    let (job, directory) = report(manager, 2).await;
    manager.follow_taken_jobs().await.unwrap();
    assert!(directory.join("prepared").exists());
    let workspace = manager.workspace(&job.workspace_id).await.unwrap();
    assert_eq!(
        git(&workspace.path, &["rev-parse", "HEAD"]).trim(),
        fixture.topic
    );
    assert_eq!(workspace.at.as_ref().unwrap().commit, fixture.topic);
    let claimed = manager.claimed_jobs().await.unwrap().remove(0);
    assert_eq!((claimed.job.job.id, claimed.job.job.run), (21, 2));
    assert!(claimed.job.run.url.ends_with("/actions/runs/2"));
}

#[tokio::test]
async fn a_workspace_that_cannot_move_fails_the_job() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    fixture.state.lock().unwrap().queued = vec![github_job(11, 1)];
    let (job, directory) = report(manager, 3).await;
    manager.follow_taken_jobs().await.unwrap();
    let refused = std::fs::read_to_string(directory.join("refused")).unwrap();
    assert!(refused.starts_with("Shoal could not move"), "{refused}");
    let workspace = manager.workspace(&job.workspace_id).await.unwrap();
    assert_eq!(
        git(&workspace.path, &["rev-parse", "HEAD"]).trim(),
        fixture.head
    );
}
