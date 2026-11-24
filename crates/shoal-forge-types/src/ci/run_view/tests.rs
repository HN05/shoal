use super::*;
use crate::numbers::RunId;

#[test]
fn a_failed_run_whose_only_stopped_jobs_were_cancelled_was_cancelled() {
    let job = |state| RunJob {
        id: JobId::new(1),
        name: "rust".into(),
        state,
        result: None,
        url: None,
    };
    let states = |states: &[JobState]| states.iter().copied().map(job).collect::<Vec<_>>();
    assert_eq!(
        stopped_run_state(&states(&[JobState::Finished, JobState::Cancelled])),
        JobState::Cancelled
    );
    assert_eq!(
        stopped_run_state(&states(&[JobState::Failed, JobState::Cancelled])),
        JobState::Failed
    );
    assert_eq!(
        stopped_run_state(&states(&[JobState::Finished])),
        JobState::Failed
    );
}

#[test]
fn a_stopped_run_takes_the_result_its_jobs_show() {
    let job = |state, result| RunJob {
        id: JobId::new(1),
        name: "rust".into(),
        state,
        result: Some(result),
        url: None,
    };
    let failed = job(JobState::Failed, CheckState::Failure);
    let cancelled = job(JobState::Cancelled, CheckState::Cancelled);
    let passed = job(JobState::Finished, CheckState::Success);
    let result = |reported, jobs: Vec<RunJob>| {
        let run = Run {
            id: RunId::new(1),
            url: String::new(),
            branch: None,
            workflow: None,
            commit: None,
            pull: None,
            state: JobState::Failed,
            result: Some(reported),
            waiting: false,
            finish: None,
        };
        RunView::new(run, jobs).run.result
    };
    // A failed job cancelled the others, and the forge reports the run cancelled.
    assert_eq!(
        result(
            CheckState::Cancelled,
            vec![failed.clone(), cancelled.clone()]
        ),
        Some(CheckState::Failure)
    );
    assert_eq!(
        result(CheckState::Failure, vec![passed, cancelled]),
        Some(CheckState::Cancelled)
    );
    assert_eq!(
        result(CheckState::TimedOut, vec![failed]),
        Some(CheckState::TimedOut)
    );
}

#[test]
fn a_run_fails_still_only_as_the_newest_of_its_workflow_on_its_ref() {
    let run = |id, workflow: &str, branch: &str, state| Run {
        id: RunId::new(id),
        url: format!("https://forge.example/team/app/actions/runs/{id}"),
        branch: Some(branch.into()),
        workflow: Some(workflow.into()),
        commit: None,
        pull: None,
        state,
        result: None,
        waiting: false,
        finish: None,
    };
    let newest_first = vec![
        run(6, "ci.yml", "main", JobState::Finished),
        run(5, "release.yml", "v1.4.0", JobState::Failed),
        run(4, "ci.yml", "fix", JobState::Failed),
        run(3, "ci.yml", "main", JobState::Failed),
        run(2, "ci.yml", "fix", JobState::Failed),
        run(1, "release.yml", "v1.3.0", JobState::Cancelled),
    ];
    let failing: Vec<_> = (still_failing(newest_first).into_iter())
        .map(|run| run.id.get())
        .collect();
    assert_eq!(failing, [5, 4]);
}
