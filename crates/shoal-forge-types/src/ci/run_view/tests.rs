use super::*;

#[test]
fn a_failed_run_whose_only_stopped_jobs_were_cancelled_was_cancelled() {
    let job = |state| RunJob {
        id: JobId::new(1),
        name: "rust".into(),
        state,
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
