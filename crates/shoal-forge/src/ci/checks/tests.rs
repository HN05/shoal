use super::*;
use crate::numbers::JobId;

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
    assert!(github.failed_job(&checks, "docs").is_err());
    assert!(github.failed_job(&checks, "lint").is_err());
    let error = github.failed_job(&checks, "coverage").unwrap_err();
    assert!(error.downcast_ref::<NoJobLog>().is_some(), "{error}");

    let gitlab = ForgeRepo::parse("https://gitlab.com/group/repo").unwrap();
    let location = Location::GitLab {
        job: JobId::new(56),
    };
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
fn tails_are_bounded_and_keep_the_failed_step() {
    let text = format!("{}\nJob failed\n", "🦀".repeat(TAIL_BYTES));
    let log = CheckLog::from_text("pr", "rust", &text, Some("Test".into()));
    assert!(log.truncated);
    assert!(log.text.len() <= TAIL_BYTES);
    assert_eq!(log.failed_step.as_deref(), Some("Test"));
    assert!(log.text.ends_with("Job failed\n"));
    assert!(!CheckLog::from_text("pr", "rust", "short", None).truncated);
    let long = CheckLog::from_text("pr", "rust", &"\0".repeat(TAIL_BYTES * 2), None);
    assert_eq!(long.text.len(), TAIL_BYTES);
    let job = |length| CheckLog::from_text("pr", "rust", "", None).with_job("j".repeat(length));
    assert!(job(JOB_URL_BYTES).job_url.is_some());
    assert!(job(JOB_URL_BYTES + 1).job_url.is_none());
}

#[test]
fn a_failed_check_update_names_the_failed_step_or_why_its_log_is_unavailable() {
    let log = |step: Option<&str>| Ok(CheckLog::from_text("pr", "rust", "", step.map(Into::into)));
    assert!(
        failed_log_summary(log(Some("Test")))
            .unwrap()
            .contains("Test")
    );
    assert_eq!(failed_log_summary(log(None)).as_deref(), Some(""));
    let external = anyhow::Error::new(NoJobLog {
        name: "coverage".into(),
    });
    assert_eq!(failed_log_summary(Err(external)), None);
    let summary = failed_log_summary(Err(anyhow::anyhow!("{}", "x".repeat(1000)))).unwrap();
    assert!(summary.contains(&"x".repeat(MAX_LOG_ERROR)));
    assert!(!summary.contains(&"x".repeat(MAX_LOG_ERROR + 1)));
}

#[tokio::test]
async fn failed_logs_leave_out_another_repositorys_prs() {
    let forge = ForgeRepo::parse("https://github.com/team/repo").unwrap();
    let view = serde_json::json!({
        "url": "https://github.com/them/tool/pull/7", "kind": "pr",
        "number": 7, "title": "Fix", "state": "open", "author": "them",
        "created_at": "", "labels": [], "body": "",
        "pr": {"head": "fix", "base": "main", "draft": false, "merge_conflicts": null,
            "checks": [{"name": "rust", "result": "failure"}], "review": null},
    });
    let mut views = [serde_json::from_value::<ItemView>(view).unwrap()];
    let account = crate::account::Account::user(crate::account::Logins::default());
    forge.read_failed_logs(&account, &mut views).await;
    assert!(views[0].errors.is_empty(), "{:?}", views[0].errors);
    assert!(views[0].check_logs.is_empty());
}
