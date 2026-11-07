use super::*;
use crate::daemon::workspace::{
    ExecutionKind,
    runner::tests::{GitHubState, admit_and_claim, github_fixture, github_job, launched},
};

/// Make the claimed job `id` look launched `seconds` ago.
async fn launched_ago(manager: &Manager, id: &str, seconds: u64) {
    let at = crate::time::UnixTime::now() - std::time::Duration::from_secs(seconds);
    let id = id.to_owned();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE ci_jobs SET launched_at=?2 WHERE id=?1",
                params![id, at],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

fn deleted(state: &std::sync::Mutex<GitHubState>) -> Vec<String> {
    state.lock().unwrap().deleted.clone()
}

#[tokio::test]
async fn a_jobs_runner_runs_from_its_copy_and_goes_once_no_job_is_left_for_it() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    fixture.state.lock().unwrap().queued = vec![github_job(11, 1)];
    admit_and_claim(manager).await;
    let claimed = manager.claimed_jobs().await.unwrap();
    let [job] = &claimed[..] else {
        panic!("{claimed:?}");
    };
    assert_eq!(job.runner_id, Some(77));
    let workspace = manager.workspace_by_id(&job.workspace_id).await.unwrap();
    assert_eq!(workspace.at.as_ref().unwrap().commit, fixture.head);
    let (command, started) = launched(&workspace.id);
    let copy = manager.job_directory(&job.id).join("runner");
    assert_eq!(command.argv, [copy.join("run.sh").into_os_string()]);
    assert!(copy.join("run.sh").exists() && copy.join("bin/Runner.Listener").exists());
    assert_eq!(
        std::fs::read_link(copy.join("_work/repo/repo")).unwrap(),
        workspace.path
    );

    // A runner just started waits for the forge to give it its job.
    manager.release_idle_runners().await.unwrap();
    launched_ago(manager, &job.id, 120).await;
    // One that runs a job, or has one left to take, stays.
    fixture.state.lock().unwrap().busy = Some(true);
    manager.release_idle_runners().await.unwrap();
    fixture.state.lock().unwrap().busy = Some(false);
    manager.release_idle_runners().await.unwrap();
    assert!(deleted(&fixture.state).is_empty());
    // The job waits for labels the runner was not registered with, as after
    // a change of the configured ones, so this runner goes.
    fixture.state.lock().unwrap().labels = vec!["shoal-old"];
    manager.release_idle_runners().await.unwrap();
    assert_eq!(deleted(&fixture.state), ["77"]);

    manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    manager.finish_ended_jobs().await.unwrap();
    assert!(manager.workspace_by_id(&workspace.id).await.is_err());
    // Its end deletes the runner too, which counts as done once it is gone.
    assert_eq!(deleted(&fixture.state), ["77", "77"]);
}

#[tokio::test]
async fn a_runner_that_took_a_job_stays_until_the_job_ends() {
    let fixture = github_fixture().await;
    let manager = &fixture.manager;
    fixture.state.lock().unwrap().queued = vec![github_job(11, 1)];
    admit_and_claim(manager).await;
    let job = manager.claimed_jobs().await.unwrap().remove(0);
    let (_, started) = launched(&job.workspace_id);
    launched_ago(manager, &job.id, 120).await;
    // Its hook reported the job it took, which the forge no longer lists.
    let directory = manager.job_directory(&job.id);
    std::fs::write(directory.join("took-run"), "1\n").unwrap();
    fixture.state.lock().unwrap().queued.clear();
    manager.release_idle_runners().await.unwrap();
    assert!(deleted(&fixture.state).is_empty());

    manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    manager.finish_ended_jobs().await.unwrap();
    assert_eq!(deleted(&fixture.state), ["77"]);
}
