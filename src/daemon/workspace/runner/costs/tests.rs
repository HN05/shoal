use serde_json::json;

use super::*;
use crate::test_support::{manager, repository};

fn queued(workflow: &str, name: &str) -> QueuedJob {
    serde_json::from_value(json!({
        "job": {"handle": "h", "id": 1, "run": 1, "name": name, "labels": ["shoal"]},
        "run": {"url": "", "workflow": workflow, "commit": "abc", "trigger": "other"},
    }))
    .unwrap()
}

#[tokio::test]
async fn a_job_counts_at_the_peaks_of_its_recent_runs() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let rust = queued("ci.yml", "rust");
    assert_eq!(
        manager.job_cost(&repo.id, &rust).await.unwrap(),
        JobCost::UNKNOWN
    );

    // Six runs end; the first, the busiest, is older than the recent five.
    let gib = 1_i64 << 30;
    for (run, (cpu, memory)) in [(900.0, 9 * gib), (300.0, gib), (400.0, 3 * gib)]
        .into_iter()
        .chain([(200.0, gib); 3])
        .enumerate()
    {
        let workspace = manager
            .create_workspace(&repo.id, format!("ci-{run}"), None, None, None)
            .await
            .unwrap();
        let job = ClaimedJob {
            id: format!("job-{run}"),
            repository_id: repo.id.clone(),
            workspace_id: workspace.id.clone(),
            execution_id: None,
            launched_at: None,
            job: rust.clone(),
            runner_id: None,
        };
        let row = (job.id.clone(), repo.id.clone(), workspace.id.clone());
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO ci_jobs(id,repository_id,workspace_id,job,started_at,peak_cpu,peak_memory)
                     VALUES (?1,?2,?3,'{}',0,?4,?5)",
                    params![row.0, row.1, row.2, cpu, memory],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        manager.record_job_usage(&job).await.unwrap();
        // A removal tried again records the run no second time.
        manager.record_job_usage(&job).await.unwrap();
    }
    assert_eq!(
        manager.job_cost(&repo.id, &rust).await.unwrap(),
        JobCost {
            cores: 4.0,
            memory: Some(3 << 30),
        }
    );
    // Another job of the workflow has a history of its own.
    assert_eq!(
        manager
            .job_cost(&repo.id, &queued("ci.yml", "docs"))
            .await
            .unwrap(),
        JobCost::UNKNOWN
    );
    let runs: i64 = manager
        .store
        .run(|db| Ok(db.query_row("SELECT count(*) FROM ci_job_usage", [], |row| row.get(0))?))
        .await
        .unwrap();
    assert_eq!(runs, 5);
}
