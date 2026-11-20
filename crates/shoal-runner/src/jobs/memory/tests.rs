use rusqlite::params;
use serde_json::json;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::model::QueuedJob;

use super::*;
use crate::stand_in::Fixture;

/// Claim a job whose runner the execution `execution` runs, at `peak`.
async fn claim(fixture: &Fixture, execution: &str, peak: i64) -> (QueueEntryId, SpaceId) {
    let workspace = fixture.record_workspace(execution).await;
    let job: QueuedJob = serde_json::from_value(json!({
        "job": {"handle": "h", "id": 1, "run": 1, "name": "rust", "labels": ["shoal"]},
        "run": {"url": "https://forge.example/team/repo/actions/runs/31576", "workflow": "ci.yml",
            "commit": "abc", "trigger": "other"},
    }))
    .unwrap();
    let id = QueueEntryId::from(format!("job-{execution}"));
    let row = (id.clone(), fixture.repository().clone(), workspace.clone());
    let (job, execution) = (serde_json::to_string(&job).unwrap(), execution.to_owned());
    (fixture.runner.store)
        .run(move |db| {
            db.execute(
                "INSERT INTO ci_jobs(id,repository_id,workspace_id,job,started_at,peak_memory,
                     execution_id)
                 VALUES (?1,?2,?3,?4,0,?5,?6)",
                params![row.0, row.1, row.2, job, peak, execution],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (id, workspace)
}

/// The peak memory recorded for the claimed job `id`.
async fn peak(fixture: &Fixture, id: &QueueEntryId) -> i64 {
    let id = id.clone();
    (fixture.runner.store)
        .run(move |db| {
            Ok(
                db.query_row("SELECT peak_memory FROM ci_jobs WHERE id=?1", [id], |row| {
                    row.get(0)
                })?,
            )
        })
        .await
        .unwrap()
}

/// The reasons of the job ends logged in `workspace`.
async fn ends(fixture: &Fixture, workspace: &SpaceId) -> Vec<String> {
    let workspace = workspace.clone();
    (fixture.runner.store)
        .run(move |db| {
            let mut query = db.prepare(
                "SELECT json_extract(record,'$.job.reason') FROM space_events
                 WHERE space_id=?1 AND json_extract(record,'$.kind')='job_ended'",
            )?;
            Ok(query
                .query_map([workspace], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_jobs_scope_raises_its_peak_and_names_its_kills_in_its_end() {
    let fixture = Fixture::new("").await;
    let (calm, calm_space) = claim(&fixture, "calm", 3 << 30).await;
    let (killed, killed_space) = claim(&fixture, "killed", 1 << 30).await;
    let runner = &fixture.runner;

    let below = ScopeMemory {
        oom_kills: 0,
        peak: 2 << 30,
    };
    runner
        .record_job_memory(&"calm".into(), below)
        .await
        .unwrap();
    assert_eq!(peak(&fixture, &calm).await, 3 << 30);
    assert!(ends(&fixture, &calm_space).await.is_empty());

    let killed_memory = ScopeMemory {
        oom_kills: 2,
        peak: 5 << 30,
    };
    runner
        .record_job_memory(&"killed".into(), killed_memory)
        .await
        .unwrap();
    assert_eq!(peak(&fixture, &killed).await, 5 << 30);
    let ends = ends(&fixture, &killed_space).await;
    assert_eq!(ends.len(), 1);
    assert!(ends[0].contains("killed 2"), "{ends:?}");

    // An execution that ran no job records nothing.
    runner
        .record_job_memory(&"agent".into(), killed_memory)
        .await
        .unwrap();
}
