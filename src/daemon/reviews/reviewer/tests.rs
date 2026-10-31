use super::*;

fn files() -> (tempfile::TempDir, Files) {
    let root = tempfile::tempdir().unwrap();
    let files = Files {
        directory: root.path().to_owned(),
    };
    fs::create_dir(files.checkout()).unwrap();
    (root, files)
}

fn reviewer<'a>(files: &'a Files, argv: &[&str]) -> Reviewer<'a> {
    Reviewer {
        argv: argv.iter().map(OsString::from).collect(),
        environment: BTreeMap::new(),
        remove: BTreeSet::new(),
        files,
        writes_output: false,
        format: &output::PLAIN,
    }
}

#[tokio::test(start_paused = true)]
async fn a_reviewer_running_past_the_limit_times_out() {
    let (_root, files) = files();
    let (_cancel, mut cancelled) = watch::channel(false);
    let ended = run(&reviewer(&files, &["sleep", "100000"]), &mut cancelled).await;
    assert_eq!(ended, Ended::Failed(FailureReason::TimedOut));
    assert_eq!(TIMEOUT, Duration::from_secs(30 * 60));
}

#[tokio::test]
async fn a_cancelled_reviewer_is_stopped() {
    let (_root, files) = files();
    let (cancel, mut cancelled) = watch::channel(false);
    cancel.send_replace(true);
    let ended = run(&reviewer(&files, &["sleep", "100000"]), &mut cancelled).await;
    assert_eq!(ended, Ended::Cancelled);
}

#[tokio::test]
async fn the_result_comes_from_the_output_file_when_the_reviewer_writes_one() {
    let (_root, files) = files();
    let (_cancel, mut cancelled) = watch::channel(false);
    let review = r#"{"summary":"No issues found.","verdict":"approve","findings":[],"checked":[]}"#;
    let script = format!("printf '%s' '{review}' > '{}'", files.output().display());
    let mut writer = reviewer(&files, &["sh", "-c", &script]);
    writer.writes_output = true;
    let Ended::Returned(output) = run(&writer, &mut cancelled).await else {
        panic!("no review returned");
    };
    assert!(output.findings.is_empty());
    let missing = reviewer(&files, &["/nonexistent/reviewer"]);
    assert_eq!(
        run(&missing, &mut cancelled).await,
        Ended::Failed(FailureReason::ExecutionFailed)
    );
}
