use std::time::Duration;

use serde_json::json;
use shoal_daemon_base::jobs::JobCost;

use super::*;
use crate::stand_in::Fixture;

fn queued(workflow: &str, name: &str) -> QueuedJob {
    serde_json::from_value(json!({
        "job": {"handle": "h", "id": 1, "run": 1, "name": name, "labels": ["shoal"]},
        "run": {"url": "https://forge.example/team/repo/actions/runs/1", "workflow": workflow, "commit": "abc", "trigger": "other"},
    }))
    .unwrap()
}

/// A claimed run `n` of `job` whose workspace reached `memory` at its peak.
async fn claimed(fixture: &Fixture, n: usize, job: &QueuedJob, memory: i64) -> ClaimedJob {
    let workspace = fixture.record_workspace(&format!("ci-{n}")).await;
    let claimed = ClaimedJob {
        id: format!("job-{n}").into(),
        repository_id: fixture.repository().clone(),
        workspace_id: workspace,
        execution_id: None,
        launched_at: None,
        job: job.clone(),
        runner_id: None,
    };
    let row = (claimed.id.clone(), claimed.repository_id.clone());
    let workspace = claimed.workspace_id.clone();
    (fixture.runner.store)
        .run(move |db| {
            db.execute(
                "INSERT INTO ci_jobs(id,repository_id,workspace_id,job,started_at,peak_memory)
                 VALUES (?1,?2,?3,'{}',0,?4)",
                params![row.0, row.1, workspace, memory],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    claimed
}

const GIB: i64 = 1 << 30;

/// Record that runs of `job` ended with the peaks `memory`, the last one
/// latest, numbering them from `first`.
async fn end_runs(fixture: &Fixture, first: usize, job: &QueuedJob, memory: &[i64]) {
    for (n, memory) in memory.iter().enumerate() {
        let claimed = claimed(fixture, first + n, job, *memory).await;
        fixture.runner.record_job_usage(&claimed).await.unwrap();
        // A removal tried again records the run no second time.
        fixture.runner.record_job_usage(&claimed).await.unwrap();
    }
}

/// Have every run recorded so far end `age` earlier than it did.
async fn age_runs(fixture: &Fixture, age: Duration) {
    let seconds = i64::try_from(age.as_secs()).unwrap();
    (fixture.runner.store)
        .run(move |db| {
            db.execute("UPDATE ci_job_usage SET ended_at=ended_at-?1", [seconds])?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn cost(fixture: &Fixture, job: &QueuedJob) -> Option<u64> {
    let repository = fixture.repository();
    fixture
        .runner
        .job_cost(repository, job)
        .await
        .unwrap()
        .memory
}

async fn kept_runs(fixture: &Fixture) -> i64 {
    (fixture.runner.store)
        .run(|db| Ok(db.query_row("SELECT count(*) FROM ci_job_usage", [], |row| row.get(0))?))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_job_counts_at_the_peak_memory_of_its_runs_in_the_window() {
    let fixture = Fixture::new("").await;
    let rust = queued("ci.yml", "rust");
    assert_eq!(
        (fixture.runner.job_cost(fixture.repository(), &rust))
            .await
            .unwrap(),
        JobCost { memory: None }
    );

    // A whole run, then more runs cut short than the latest runs counted.
    end_runs(&fixture, 0, &rust, &[9 * GIB]).await;
    end_runs(&fixture, 1, &rust, &[GIB; 6]).await;
    assert_eq!(cost(&fixture, &rust).await, Some(9 << 30));
    // Another job of the workflow has a history of its own.
    assert_eq!(cost(&fixture, &queued("ci.yml", "docs")).await, None);
}

#[tokio::test]
async fn a_peak_counts_until_it_ages_out_of_the_window() {
    let fixture = Fixture::new("").await;
    let rust = queued("ci.yml", "rust");
    end_runs(&fixture, 0, &rust, &[9 * GIB, 3 * GIB]).await;
    age_runs(&fixture, RECENT_FOR + Duration::from_secs(60)).await;
    // Past the window, the latest runs still count, whatever their age.
    assert_eq!(cost(&fixture, &rust).await, Some(9 << 30));

    end_runs(&fixture, 2, &rust, &[GIB; 4]).await;
    assert_eq!(cost(&fixture, &rust).await, Some(3 << 30));
    assert_eq!(kept_runs(&fixture).await, 5, "the oldest run is forgotten");
    end_runs(&fixture, 6, &rust, &[GIB]).await;
    assert_eq!(cost(&fixture, &rust).await, Some(GIB as u64));
    assert_eq!(kept_runs(&fixture).await, 5);
}
