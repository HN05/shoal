use serde_json::json;

use super::*;
use crate::daemon::test_support::{manager, repository};
use std::time::Duration;

fn queued(workflow: &str, name: &str) -> QueuedJob {
    serde_json::from_value(json!({
        "job": {"handle": "h", "id": 1, "run": 1, "name": name, "labels": ["shoal"]},
        "run": {"url": "https://forge.example/team/repo/actions/runs/1", "workflow": workflow, "commit": "abc", "trigger": "other"},
    }))
    .unwrap()
}

#[tokio::test]
async fn a_job_counts_at_the_peak_memory_of_its_recent_runs() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let rust = queued("ci.yml", "rust");
    assert_eq!(
        manager.job_cost(&repo.id, &rust).await.unwrap(),
        JobCost { memory: None }
    );

    // Six runs end; the first, the busiest, is older than the recent five.
    let gib = 1_i64 << 30;
    for (run, memory) in [9 * gib, gib, 3 * gib]
        .into_iter()
        .chain([gib; 3])
        .enumerate()
    {
        let workspace = manager
            .create_workspace(&repo.id, format!("ci-{run}"), None, None, None)
            .await
            .unwrap();
        let job = ClaimedJob {
            id: format!("job-{run}").into(),
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
                    "INSERT INTO ci_jobs(id,repository_id,workspace_id,job,started_at,peak_memory)
                     VALUES (?1,?2,?3,'{}',0,?4)",
                    params![row.0, row.1, row.2, memory],
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
            memory: Some(3 << 30),
        }
    );
    // Another job of the workflow has a history of its own.
    assert_eq!(
        manager
            .job_cost(&repo.id, &queued("ci.yml", "docs"))
            .await
            .unwrap(),
        JobCost { memory: None }
    );
    let runs: i64 = manager
        .store
        .run(|db| Ok(db.query_row("SELECT count(*) FROM ci_job_usage", [], |row| row.get(0))?))
        .await
        .unwrap();
    assert_eq!(runs, 5);
}

/// Record a claimed job of `repo` in a new workspace named `name`, its run
/// `job`, started `ran` before now.
async fn claim(
    manager: &Manager,
    repo: &RepositoryId,
    name: &str,
    job: &QueuedJob,
    ran: Duration,
) -> ClaimedJob {
    let workspace = manager
        .create_workspace(repo, name.into(), None, None, None)
        .await
        .unwrap();
    let launched = crate::time::UnixTime::now() - ran;
    let claimed = ClaimedJob {
        id: format!("job-{name}").into(),
        repository_id: repo.clone(),
        workspace_id: workspace.id.clone(),
        execution_id: None,
        launched_at: Some(launched),
        job: job.clone(),
        runner_id: None,
    };
    let row = (
        claimed.id.clone(),
        repo.clone(),
        workspace.id,
        serde_json::to_string(job).unwrap(),
    );
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO ci_jobs(id,repository_id,workspace_id,job,started_at,launched_at,peak_memory)
                 VALUES (?1,?2,?3,?4,0,?5,1)",
                params![row.0, row.1, row.2, row.3, launched],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    claimed
}

#[tokio::test]
async fn a_spaces_ci_has_what_its_longest_job_has_left_once_each_has_run() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let space: SpaceId = "space".into();
    let mut rust = queued("ci.yml", "rust");
    rust.space_ids = vec![space.clone()];
    // Nothing runs for the space yet.
    let now = crate::time::UnixTime::now();
    assert_eq!(manager.ci_remaining(&space, now).await.unwrap(), None);

    // A run of the job ends after half an hour.
    let earlier = claim(
        &manager,
        &repo.id,
        "earlier",
        &rust,
        Duration::from_secs(1800),
    )
    .await;
    manager.record_job_usage(&earlier).await.unwrap();
    let ran = crate::time::UnixTime::now().duration_since(earlier.launched_at.unwrap());
    let seconds: i64 = manager
        .store
        .run(|db| Ok(db.query_row("SELECT seconds FROM ci_job_usage", [], |row| row.get(0))?))
        .await
        .unwrap();
    // It ran from its recorded start until it ended, before `ran` was read.
    let ran = i64::try_from(ran.as_secs()).unwrap();
    assert!((1800..=ran).contains(&seconds), "{seconds}");
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE ci_job_usage SET seconds=1800", [])?;
            db.execute("DELETE FROM ci_jobs", [])?;
            Ok(())
        })
        .await
        .unwrap();

    // Another run of it, ten minutes in, has twenty left.
    let running = claim(&manager, &repo.id, "running", &rust, Duration::ZERO).await;
    let now = running.launched_at.unwrap() + Duration::from_secs(600);
    assert_eq!(
        manager.ci_remaining(&space, now).await.unwrap(),
        Some(Duration::from_secs(1200))
    );
    // A job that has never run leaves the wait unknown.
    let mut docs = queued("ci.yml", "docs");
    docs.space_ids = vec![space.clone()];
    claim(&manager, &repo.id, "docs", &docs, Duration::ZERO).await;
    assert_eq!(manager.ci_remaining(&space, now).await.unwrap(), None);
}
