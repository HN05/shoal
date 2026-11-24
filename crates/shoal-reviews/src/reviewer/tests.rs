use super::*;
use crate::test_support::until_tests_end;

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
        runs: Box::leak(Box::default()),
        timeout: TIMEOUT,
        command: AgentCommand {
            argv: argv.iter().map(std::ffi::OsString::from).collect(),
            environment: Default::default(),
            remove: Default::default(),
        },
        files,
        writes_output: false,
        format: &output::PLAIN,
    }
}

#[tokio::test]
async fn a_reviewer_running_past_the_limit_times_out() {
    let (_root, files) = files();
    let (_cancel, mut cancelled) = watch::channel(false);
    let mut slow = reviewer(&files, &["sh", "-c", &until_tests_end()]);
    slow.timeout = Duration::from_millis(300);
    let ended = run(&slow, &mut cancelled).await;
    assert_eq!(ended, Ended::Failed(FailureReason::TimedOut));
}

#[tokio::test]
async fn a_reviewer_whose_processes_cannot_be_proven_stopped_is_left_running() {
    let (_root, files) = files();
    let (_cancel, mut cancelled) = watch::channel(false);
    let reviewer = reviewer(&files, &["true"]);
    reviewer
        .runs
        .fail_inventory
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let ended = run(&reviewer, &mut cancelled).await;
    assert_eq!(ended, Ended::LeftRunning);
    assert!(files.directory.exists());
}

#[tokio::test]
async fn the_result_comes_from_the_output_file_when_the_reviewer_writes_one() {
    let (_root, files) = files();
    let (_cancel, mut cancelled) = watch::channel(false);
    let result = r#"{"session_id":"4f2c","result":"Posted."}"#;
    let script = format!("printf '%s' '{result}' > '{}'", files.output().display());
    let mut writer = reviewer(&files, &["sh", "-c", &script]);
    (writer.writes_output, writer.format) = (true, &output::CLAUDE);
    assert_eq!(
        run(&writer, &mut cancelled).await,
        Ended::Returned {
            session: Some("4f2c".into())
        }
    );
    let missing = reviewer(&files, &["/nonexistent/reviewer"]);
    assert_eq!(
        run(&missing, &mut cancelled).await,
        Ended::Failed(FailureReason::ExecutionFailed)
    );
}

#[test]
fn a_run_produced_what_it_was_asked_for_only_by_writing_it() {
    let exited = || Ended::Returned { session: None };
    let failed = || Ended::Failed(FailureReason::ExecutionFailed);
    assert_eq!(
        settled(exited(), false),
        Ended::Failed(FailureReason::NotSubmitted)
    );
    assert_eq!(settled(failed(), true), exited());
    assert_eq!(settled(failed(), false), failed());
    assert_eq!(settled(Ended::Cancelled, false), Ended::Cancelled);
}
