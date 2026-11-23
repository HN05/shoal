use super::*;
use crate::stand_in::Fixture;

/// Record the job `id` of a run to rerun, cut off by `cut_off_by`, none
/// for a stop of load protection's.
async fn record(fixture: &Fixture, id: i64, cut_off_by: Option<&str>) {
    let cut_off_by = cut_off_by.map(str::to_owned);
    (fixture.runner.store)
        .write(move |tx| {
            tx.execute(
                "INSERT INTO ci_job_reruns(run_url,job_id,job_name,repository_id,stopped_at,cut_off_by)
                 VALUES ('https://forge.example/team/repo/actions/runs/7',?1,?2,'repo',?3,?4)",
                params![id, format!("job {id}"), UnixTime::now(), cut_off_by],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The names of the jobs waiting to rerun here.
async fn waiting(fixture: &Fixture) -> Vec<String> {
    let stopped = fixture.runner.stopped_jobs().await.unwrap();
    let mut names: Vec<_> = stopped.into_iter().map(|job| job.name).collect();
    names.sort();
    names
}

#[tokio::test]
async fn the_hub_takes_the_jobs_cut_off_and_this_machine_forgets_them() {
    let fixture = Fixture::new("").await;
    record(&fixture, 1, Some("the machine going to sleep")).await;
    record(&fixture, 2, None).await;
    *fixture.daemon.handed.lock().unwrap() = Some(Vec::new());
    (fixture.runner.hand_over_cut_off_jobs(&*fixture.daemon))
        .await
        .unwrap();
    let handed = fixture.daemon.handed.lock().unwrap().clone().unwrap();
    let handed: Vec<_> = handed.into_iter().map(|job| job.name).collect();
    assert_eq!(handed, ["job 1"]);
    assert_eq!(waiting(&fixture).await, ["job 2"]);
}

#[tokio::test]
async fn a_machine_in_no_hub_keeps_the_jobs_cut_off() {
    let fixture = Fixture::new("").await;
    record(&fixture, 1, Some("the machine going to sleep")).await;
    (fixture.runner.hand_over_cut_off_jobs(&*fixture.daemon))
        .await
        .unwrap();
    assert_eq!(waiting(&fixture).await, ["job 1"]);
}
