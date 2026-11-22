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

#[tokio::test]
async fn a_job_counts_at_the_peak_memory_of_its_recent_runs() {
    let fixture = Fixture::new("").await;
    let runner = &fixture.runner;
    let repository = fixture.repository();
    let rust = queued("ci.yml", "rust");
    assert_eq!(
        runner.job_cost(repository, &rust).await.unwrap(),
        JobCost { memory: None }
    );

    // Six runs end; the first, the busiest, is older than the recent five.
    let gib = 1_i64 << 30;
    for (run, memory) in [9 * gib, gib, 3 * gib]
        .into_iter()
        .chain([gib; 3])
        .enumerate()
    {
        let job = claimed(&fixture, run, &rust, memory).await;
        runner.record_job_usage(&job).await.unwrap();
        // A removal tried again records the run no second time.
        runner.record_job_usage(&job).await.unwrap();
    }
    assert_eq!(
        runner.job_cost(repository, &rust).await.unwrap(),
        JobCost {
            memory: Some(3 << 30),
        }
    );
    // Another job of the workflow has a history of its own.
    assert_eq!(
        (runner.job_cost(repository, &queued("ci.yml", "docs")))
            .await
            .unwrap(),
        JobCost { memory: None }
    );
    let runs: i64 = (runner.store)
        .run(|db| Ok(db.query_row("SELECT count(*) FROM ci_job_usage", [], |row| row.get(0))?))
        .await
        .unwrap();
    assert_eq!(runs, 5);
}
