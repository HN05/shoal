use serde_json::json;

use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::{
    test_support::git,
    workspace::runner::tests::{admit_and_claim, github_fixture, github_job, launched},
};
use crate::forge::numbers::JobId;

/// Claim GitHub's job 11 of run 1 and report, as its runner's hook does,
/// that the runner took a job of `run`, returning the claim and the job's
/// directory.
async fn report(manager: &Manager, run: u64) -> (ClaimedJob, std::path::PathBuf) {
    admit_and_claim(manager).await;
    let job = manager.claimed_jobs().await.unwrap().remove(0);
    launched(job.workspace_id.as_str());
    let directory = manager.job_directory(&job.id);
    // The hook holds a job of another run than the claimed one back.
    if run != 1 {
        std::fs::write(directory.join("awaits"), "").unwrap();
    }
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
        (JobId::new(12), "docs")
    );
    let workspace = manager.workspace_by_id(&job.workspace_id).await.unwrap();
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
    let workspace = manager.workspace_by_id(&job.workspace_id).await.unwrap();
    assert_eq!(
        git(&workspace.path, &["rev-parse", "HEAD"]).trim(),
        fixture.topic
    );
    assert_eq!(workspace.at.as_ref().unwrap().commit, fixture.topic);
    let claimed = manager.claimed_jobs().await.unwrap().remove(0);
    assert_eq!(
        (claimed.job.job.id, claimed.job.job.run),
        (JobId::new(21), RunId::new(2))
    );
    assert!(claimed.job.run.url.ends_with("/actions/runs/2"));
}

#[tokio::test]
async fn a_workspace_that_cannot_move_fails_the_job() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    fixture.state.lock().unwrap().queued = vec![github_job(11, 1)];
    let (job, directory) = report(manager, 3).await;
    manager.follow_taken_jobs().await.unwrap();
    assert!(directory.join("refused").exists());
    // Run 3 cannot be read, and stopping the job still cancels it.
    let claimed = manager.claimed_jobs().await.unwrap().remove(0);
    assert!(claimed.job.run.url.ends_with("/actions/runs/3"));
    let workspace = manager.workspace_by_id(&job.workspace_id).await.unwrap();
    assert_eq!(
        git(&workspace.path, &["rev-parse", "HEAD"]).trim(),
        fixture.head
    );
}

#[tokio::test]
async fn a_job_of_the_claimed_run_keeps_the_checkout_its_steps_made() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    fixture.state.lock().unwrap().queued = vec![github_job(11, 1)];
    let (job, directory) = report(manager, 1).await;
    let workspace = manager.workspace_by_id(&job.workspace_id).await.unwrap();
    git(
        &workspace.path,
        &["checkout", "-q", "--detach", &fixture.topic],
    );
    manager.follow_taken_jobs().await.unwrap();
    assert!(directory.join("prepared").exists());
    assert_eq!(
        git(&workspace.path, &["rev-parse", "HEAD"]).trim(),
        fixture.topic
    );
}

#[tokio::test]
async fn a_job_whose_commit_cannot_be_fetched_still_names_its_run() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    fixture.state.lock().unwrap().queued = vec![github_job(11, 1)];
    let (job, directory) = report(manager, 4).await;
    manager.follow_taken_jobs().await.unwrap();
    assert!(directory.join("refused").exists());
    // Stopping the job cancels the run it took, not the claimed one.
    let claimed = manager.claimed_jobs().await.unwrap().remove(0);
    assert!(claimed.job.run.url.ends_with("/actions/runs/4"));
    let workspace = manager.workspace_by_id(&job.workspace_id).await.unwrap();
    assert_eq!(
        git(&workspace.path, &["rev-parse", "HEAD"]).trim(),
        fixture.head
    );
}

#[tokio::test]
async fn a_job_whose_name_cannot_be_read_still_runs_at_its_runs_commit() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    fixture.state.lock().unwrap().queued = vec![github_job(11, 1)];
    let (job, directory) = report(manager, 5).await;
    manager.follow_taken_jobs().await.unwrap();
    assert!(directory.join("prepared").exists());
    let claimed = manager.claimed_jobs().await.unwrap().remove(0);
    assert!(claimed.job.run.url.ends_with("/actions/runs/5"));
    assert_eq!(claimed.job.job.id, JobId::new(11));
    let workspace = manager.workspace_by_id(&job.workspace_id).await.unwrap();
    assert_eq!(
        git(&workspace.path, &["rev-parse", "HEAD"]).trim(),
        fixture.topic
    );
}

#[tokio::test]
async fn a_report_recorded_before_a_restart_still_moves_the_workspace() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    {
        let mut state = fixture.state.lock().unwrap();
        state.queued = vec![github_job(11, 1)];
        state.taken.insert(2, taken(21, 2, "rust"));
    }
    let (job, directory) = report(manager, 2).await;
    // A restart cut the follow short after it recorded run 2's job.
    let mut recorded = job.job.clone();
    let forge = ForgeRepo::resolve_run_url(&job.job.run.url, &manager.forge_hosts())
        .await
        .unwrap();
    recorded.run = forge
        .job_run(&manager.user_account(), RunId::new(2))
        .await
        .unwrap();
    manager.record_taken_job(&job.id, &recorded).await.unwrap();

    manager.follow_taken_jobs().await.unwrap();
    assert!(directory.join("prepared").exists());
    let workspace = manager.workspace_by_id(&job.workspace_id).await.unwrap();
    assert_eq!(
        git(&workspace.path, &["rev-parse", "HEAD"]).trim(),
        fixture.topic
    );
}

#[tokio::test]
async fn stopping_a_job_before_its_report_is_followed_cancels_the_reported_run() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    fixture.state.lock().unwrap().queued = vec![github_job(11, 1)];
    let (job, _) = report(manager, 2).await;
    let cancelled = manager
        .cancel_job_in(&SpaceSelector::id(&job.workspace_id))
        .await
        .unwrap();
    assert!(cancelled.unwrap().ends_with("/actions/runs/2"));
    assert_eq!(fixture.state.lock().unwrap().cancelled, ["2"]);
}

#[tokio::test]
async fn a_stop_holding_the_claim_from_before_the_follow_cancels_the_reported_run() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    {
        let mut state = fixture.state.lock().unwrap();
        state.queued = vec![github_job(11, 1)];
        state.taken.insert(2, taken(21, 2, "rust"));
    }
    let (stale, _) = report(manager, 2).await;
    // The follow records run 2 and answers while the stop holds the claim
    // it read before.
    manager.follow_taken_jobs().await.unwrap();
    let cancelled = manager.cancel_job_run(&stale).await.unwrap();
    assert!(cancelled.ends_with("/actions/runs/2"));
    assert_eq!(fixture.state.lock().unwrap().cancelled, ["2"]);
}
