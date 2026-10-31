use super::*;
use crate::test_support::until_tests_end;

fn command(argv: &[&str]) -> AgentCommand {
    AgentCommand {
        argv: argv.iter().map(OsString::from).collect(),
        environment: BTreeMap::new(),
        remove: BTreeSet::new(),
    }
}

async fn run_in(
    root: &Path,
    command: &AgentCommand,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Ended> {
    run(
        &Run {
            command,
            directory: root,
            files: root,
            timeout: Duration::from_secs(60),
        },
        cancel,
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn a_run_past_its_timeout_is_stopped() {
    let root = tempfile::tempdir().unwrap();
    let (_cancel, mut cancelled) = watch::channel(false);
    let ended = run_in(
        root.path(),
        &command(&["sh", "-c", &until_tests_end()]),
        &mut cancelled,
    )
    .await;
    assert_eq!(ended.unwrap(), Ended::TimedOut);
}

#[tokio::test]
async fn a_cancelled_run_is_stopped() {
    let root = tempfile::tempdir().unwrap();
    let (cancel, mut cancelled) = watch::channel(false);
    cancel.send_replace(true);
    let ended = run_in(
        root.path(),
        &command(&["sh", "-c", &until_tests_end()]),
        &mut cancelled,
    )
    .await;
    assert_eq!(ended.unwrap(), Ended::Cancelled);
}

#[tokio::test]
async fn a_run_reports_its_exit_and_output_without_shoal_variables() {
    let root = tempfile::tempdir().unwrap();
    let (_cancel, mut cancelled) = watch::channel(false);
    let script =
        "env | grep '^SHOAL_' | grep -v '^SHOAL_ONESHOT_RUN='; echo out; echo err >&2; exit 3";
    let ended = run_in(root.path(), &command(&["sh", "-c", script]), &mut cancelled).await;
    assert_eq!(ended.unwrap(), Ended::Exited { success: false });
    assert_eq!(fs::read_to_string(stdout(root.path())).unwrap(), "out\n");
    assert_eq!(fs::read_to_string(stderr(root.path())).unwrap(), "err\n");
    let missing = run_in(
        root.path(),
        &command(&["/nonexistent/agent"]),
        &mut cancelled,
    )
    .await;
    assert!(missing.is_err());
}

#[test]
fn output_is_read_within_its_limits() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("stream");
    fs::write(&path, b"0123456789").unwrap();
    assert_eq!(read_tail(&path, 4).unwrap(), b"6789");
    assert_eq!(read_tail(&path, 64).unwrap(), b"0123456789");
    assert_eq!(
        read_bounded(&path, 10).unwrap().as_deref(),
        Some(&b"0123456789"[..])
    );
    assert_eq!(read_bounded(&path, 9).unwrap(), None);
    assert_eq!(read_bounded(&root.path().join("missing"), 9).unwrap(), None);
}
