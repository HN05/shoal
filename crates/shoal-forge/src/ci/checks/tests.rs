use super::*;

#[test]
fn logs_come_from_the_failed_job_a_check_links_where_the_forge_serves_them() {
    let github = ForgeRepo::parse("https://github.com/team/repo").unwrap();
    let job = "https://github.com/team/repo/actions/runs/12/job/34";
    let check = |name: &str, state, target: &str| (name.into(), state, target.into());
    let checks = [
        check("rust", JobState::Finished, job),
        check("rust", JobState::Failed, job),
        check("docs", JobState::Running, job),
        check("coverage", JobState::Failed, "https://coverage.example/7"),
    ];
    let (location, target) = github.failed_job(&checks, "rust").unwrap();
    assert_eq!(target, job);
    assert_eq!(
        github.kind.job_log_endpoint(location).unwrap(),
        "actions/jobs/34/logs"
    );
    let error = github.failed_job(&checks, "docs").unwrap_err();
    assert_eq!(error.to_string(), "check docs has not failed");
    let error = github.failed_job(&checks, "lint").unwrap_err();
    assert!(error.to_string().contains("was not found"), "{error}");
    let error = github.failed_job(&checks, "coverage").unwrap_err();
    assert!(error.downcast_ref::<NoJobLog>().is_some(), "{error}");

    let gitlab = ForgeRepo::parse("https://gitlab.com/group/repo").unwrap();
    let location = Location::GitLab { job: 56 };
    assert_eq!(
        gitlab.kind.job_log_endpoint(location).unwrap(),
        "jobs/56/trace"
    );
    let bitbucket = ForgeRepo::parse("https://bitbucket.org/team/repo").unwrap();
    let error = bitbucket.kind.job_log_endpoint(location).unwrap_err();
    assert!(
        error
            .downcast_ref::<crate::capability::Unsupported>()
            .is_some(),
        "{error}"
    );
}

#[test]
fn tails_are_bounded_and_retain_step_before_long_output() {
    let text = format!(
        "⭐ Run Main Test\n{}\n⚙️ [runner]: exitcode '1': failure\nJob failed\n",
        "🦀".repeat(TAIL_BYTES)
    );
    let log = CheckLog::from_text("pr", "rust", &text);
    assert!(log.truncated);
    assert!(log.text.len() <= TAIL_BYTES);
    assert_eq!(log.failed_step.as_deref(), Some("Main Test"));
    assert!(log.text.ends_with("Job failed\n"));
    assert!(!CheckLog::from_text("pr", "rust", "short").truncated);
    let long = CheckLog::from_text("pr", "rust", &"\0".repeat(TAIL_BYTES * 2));
    assert_eq!(long.text.len(), TAIL_BYTES);
    let job = |length| CheckLog::from_text("pr", "rust", "").with_job("j".repeat(length));
    assert!(job(JOB_URL_BYTES).job_url.is_some());
    assert!(job(JOB_URL_BYTES + 1).job_url.is_none());
}

#[test]
fn failed_steps_never_name_a_step_that_succeeded() {
    let unmarked = "⭐ Run Main Checkout\n  ✅  Success - Main Checkout\nrustc 1.98.1\n\
                    test result: FAILED\n⚙️ [runner]: exitcode '101': failure\n🏁  Job failed\n";
    let step = |text| CheckLog::from_text("pr", "rust", text).failed_step;
    assert_eq!(step(unmarked), None);
    let marked = "⭐ Run Main Checkout\n  ✅  Success - Main Checkout\n⭐ Run Main Test\n\
                  test result: FAILED\n  ❌  Failure - Main Test\n⚙️ [runner]: exitcode '101': failure\n";
    assert_eq!(step(marked).as_deref(), Some("Main Test"));
}
