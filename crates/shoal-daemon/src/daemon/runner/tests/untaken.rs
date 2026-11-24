//! Claimed jobs whose runner never takes them.
use super::*;
use crate::protocol::{self, Control, ExecutionEvent};

/// Claim job 11 of run 3, whose runner peaked at 6 MB, returning its
/// workspace.
async fn claimed(fixture: &Fixture) -> SpaceId {
    let manager = &fixture.manager;
    admit_and_claim(manager).await;
    let workspace = manager.runner.claimed_jobs().await.unwrap()[0]
        .workspace_id
        .clone();
    manager
        .store
        .run(|db| {
            db.execute("UPDATE ci_jobs SET peak_memory=6000000", [])?;
            Ok(())
        })
        .await
        .unwrap();
    workspace
}

/// Have the forge list job 11 no longer as waiting, and name the runner
/// with `uuid` in the log of the task it runs as.
fn taken_by(fixture: &Fixture, uuid: &str) {
    fixture.waiting.lock().unwrap().clear();
    let job = json!({"id": 11, "handle": "h11", "status": "running", "task_id": 40});
    let log = format!(
        "2026-11-24T12:00:00.0000000Z Runner {uuid} (version:v13.2.1) received task 40 of job rust, triggered by event: push\n"
    );
    let mut answers = fixture.answers.lock().unwrap();
    answers.insert(
        "GET /api/v1/repos/team/repo/actions/jobs/11".into(),
        job.to_string(),
    );
    answers.insert(
        "GET /api/v1/repos/team/repo/actions/jobs/11/logs?step=0".into(),
        log,
    );
}

/// How many runs of jobs the costs keep.
async fn usage_runs(manager: &Manager) -> i64 {
    manager
        .store
        .run(|db| Ok(db.query_row("SELECT count(*) FROM ci_job_usage", [], |row| row.get(0))?))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_runner_whose_job_another_runner_took_is_stopped_and_counts_no_run() {
    let fixture = fixture(vec![job("h11", 11, 3)], "").await;
    let manager = &fixture.manager;
    let workspace = claimed(&fixture).await;
    let ConnectedRunner {
        mut connection,
        executing,
    } = connect_runner(manager, &workspace, false).await;
    taken_by(&fixture, "other-uuid");

    let polling = tokio::spawn({
        let manager = manager.clone();
        async move { manager.runner.refresh_waiting_jobs(&*manager).await }
    });
    let control: Control = read(&mut connection).await;
    assert!(matches!(&control, Control::Stop { .. }), "{control:?}");
    let event = ExecutionEvent::Finished {
        exit_code: 143,
        memory: None,
    };
    protocol::write(connection.get_mut(), &event).await.unwrap();
    let control: Control = read(&mut connection).await;
    assert!(matches!(control, Control::Finished { .. }), "{control:?}");
    executing.await.unwrap().unwrap();
    polling.await.unwrap().unwrap();
    assert_eq!(logged_stops(manager), [JobStop::Untaken]);

    finish_ended_jobs(manager).await;
    assert!(manager.runner.claimed_jobs().await.unwrap().is_empty());
    assert!(manager.spaces.workspace_by_id(&workspace).await.is_err());
    assert_eq!(usage_runs(manager).await, 0);
}

#[tokio::test]
async fn a_runner_that_took_its_job_runs_on_and_its_taker_is_read_once() {
    let fixture = fixture(vec![job("h11", 11, 3)], "").await;
    let manager = &fixture.manager;
    claimed(&fixture).await;
    taken_by(&fixture, "runner-uuid");

    refresh_waiting_jobs(manager).await;
    refresh_waiting_jobs(manager).await;

    assert!(logged_stops(manager).is_empty());
    assert_eq!(manager.runner.claimed_jobs().await.unwrap().len(), 1);
    let reads = (fixture.server.received().iter())
        .filter(|request| request.line == "GET /api/v1/repos/team/repo/actions/jobs/11")
        .count();
    assert_eq!(reads, 1);
}
