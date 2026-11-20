use std::time::Duration;

use serde_json::json;
use shoal_ids::execution_ids::ExecutionId;

use super::*;
use crate::stand_in::Fixture;

fn queued() -> QueuedJob {
    serde_json::from_value(json!({
        "job": {"handle": "h", "id": 1, "run": 1, "name": "rust", "labels": ["shoal"]},
        "run": {"url": "https://forge.example/team/repo/actions/runs/1", "workflow": "ci.yml", "commit": "abc", "trigger": "other"},
    }))
    .unwrap()
}

/// Record `execution` as one the daemon runs in `workspace`.
fn record_execution(fixture: &Fixture, execution: &ExecutionId, workspace: &SpaceId) {
    let mut executions = fixture.daemon.executions.lock().unwrap();
    executions.ids.insert(execution.clone());
    executions.spaces.insert(workspace.clone());
}

/// How long ago the runner of each job that loads the machine started.
async fn ages(fixture: &Fixture) -> Vec<Duration> {
    let loads = fixture.runner.job_loads(&*fixture.daemon()).await.unwrap();
    loads.into_iter().map(|load| load.age).collect()
}

#[tokio::test]
async fn a_job_counts_as_just_started_until_its_runner_starts() {
    let fixture = Fixture::new("").await;
    let admitted = ("admitted".into(), fixture.repository().clone(), queued());
    fixture.daemon.admitted.lock().unwrap().push(admitted);
    assert_eq!(ages(&fixture).await, [Duration::ZERO]);
    fixture.daemon.admitted.lock().unwrap().clear();

    // Its age counts from its runner's start, however long ago it was admitted.
    let execution = ExecutionId::generate();
    let ten_minutes_ago = UnixTime::now() - Duration::from_secs(600);
    let job = queued();
    let claim = fixture
        .record_claim(
            "launched",
            &job,
            Some(ten_minutes_ago),
            Some(execution.clone()),
        )
        .await;
    record_execution(&fixture, &execution, &claim.workspace_id);
    assert!(ages(&fixture).await[0] >= Duration::from_secs(600));

    // One whose runner has not started yet has just started.
    let starting = fixture.record_claim("starting", &job, None, None).await;
    fixture.runner.jobs.begin_claim(&starting.id);
    assert_eq!(ages(&fixture).await.len(), 2);
    assert!(ages(&fixture).await.contains(&Duration::ZERO));
}

#[tokio::test]
async fn a_job_whose_runner_ended_reserves_nothing_while_its_workspace_remains() {
    let fixture = Fixture::new("").await;
    let execution = ExecutionId::generate();
    let job = queued();
    fixture
        .record_claim("ended", &job, Some(UnixTime::now()), Some(execution))
        .await;
    // The daemon no longer records the execution that ran its runner.
    assert!(ages(&fixture).await.is_empty());

    // A claim whose runner is not recorded counts only while it runs.
    let unrecorded = fixture.record_claim("unrecorded", &job, None, None).await;
    assert!(ages(&fixture).await.is_empty());
    assert!(fixture.runner.jobs.begin_claim(&unrecorded.id));
    assert_eq!(ages(&fixture).await.len(), 1);
    fixture.runner.jobs.end_claim(&unrecorded.id);
    // Or once its workspace runs an execution the next pass adopts.
    record_execution(&fixture, &ExecutionId::generate(), &unrecorded.workspace_id);
    assert_eq!(ages(&fixture).await.len(), 1);
}
