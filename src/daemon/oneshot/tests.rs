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
    run_tracked(
        &Runs::default(),
        root,
        command,
        Duration::from_secs(60),
        cancel,
    )
    .await
}

async fn run_tracked(
    runs: &Runs,
    root: &Path,
    command: &AgentCommand,
    timeout: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Ended> {
    run(
        &Run {
            runs,
            command,
            directory: root,
            files: root,
            timeout,
        },
        cancel,
    )
    .await
}

#[tokio::test]
async fn a_run_past_its_timeout_is_stopped() {
    let root = tempfile::tempdir().unwrap();
    let (_cancel, mut cancelled) = watch::channel(false);
    let runs = Runs::default();
    let slow = command(&["sh", "-c", &until_tests_end()]);
    let timeout = Duration::from_millis(300);
    let ended = run_tracked(&runs, root.path(), &slow, timeout, &mut cancelled).await;
    assert_eq!(ended.unwrap(), Ended::TimedOut);
    // What it left is proven stopped, so it is no longer tracked.
    assert!(runs.unfinished.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_run_whose_leftovers_cannot_be_proven_stopped_stays_tracked() {
    let root = tempfile::tempdir().unwrap();
    let (_cancel, mut cancelled) = watch::channel(false);
    let runs = Runs::default();
    runs.fail_inventory
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let timeout = Duration::from_secs(60);
    let error = run_tracked(
        &runs,
        root.path(),
        &command(&["true"]),
        timeout,
        &mut cancelled,
    )
    .await
    .unwrap_err();
    assert!(error.downcast_ref::<LeftRunning>().is_some(), "{error:#}");
    assert!(runs.unfinished.lock().unwrap().contains(root.path()));
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

#[tokio::test]
async fn a_descendant_that_left_the_group_is_stopped_with_the_run() {
    let root = tempfile::tempdir().unwrap();
    let pid_file = root.path().join("escaped");
    let mut escaping = command(&[
        std::env::current_exe().unwrap().to_str().unwrap(),
        "--exact",
        "daemon::oneshot::tests::escape_the_group",
        "--ignored",
    ]);
    escaping
        .environment
        .insert(ESCAPE_OWNER.into(), std::process::id().to_string().into());
    escaping
        .environment
        .insert(ESCAPE_PID_FILE.into(), pid_file.clone().into());
    let (_cancel, mut cancelled) = watch::channel(false);
    let ended = run_in(root.path(), &escaping, &mut cancelled).await;
    assert_eq!(ended.unwrap(), Ended::Exited { success: true });
    let pid: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Stopped, though not in the run's group.
    assert!(stopped(pid), "the escaped child {pid} runs on");
}

#[tokio::test]
async fn stopping_every_unfinished_run_stops_what_an_aborted_one_left() {
    let root = tempfile::tempdir().unwrap();
    let pid_file = root.path().join("escaped");
    // What a run aborted with the daemon left in another process group.
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "daemon::oneshot::tests::escape_the_group",
            "--ignored",
        ])
        .env(crate::env::ONESHOT_RUN, root.path())
        .env(ESCAPE_OWNER, std::process::id().to_string())
        .env(ESCAPE_PID_FILE, &pid_file)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let runs = Runs::default();
    runs.track(root.path());
    runs.stop_all().await;
    assert!(runs.unfinished.lock().unwrap().is_empty());
    let pid: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(stopped(pid), "the escaped child {pid} runs on");
}

/// Whether process `pid` has stopped, waiting a while for it to be reaped.
fn stopped(pid: u32) -> bool {
    let gone = || {
        crate::process::identity::capture(pid)
            .unwrap()
            .is_none_or(|identity| !crate::process::identity::alive(&identity).unwrap())
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !gone() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    gone()
}

/// The test process an escaped child outlives no longer than, and where it
/// writes its PID.
const ESCAPE_OWNER: &str = "ONESHOT_TEST_ESCAPE_OWNER";
const ESCAPE_PID_FILE: &str = "ONESHOT_TEST_ESCAPE_PID_FILE";
/// Set in the escaped child.
const ESCAPED: &str = "ONESHOT_TEST_ESCAPED";

/// Starts a child in a process group of its own, as a run's command whose
/// descendant left its group, then exits; the child sleeps until the test's
/// process exits, for at most ten minutes.
#[test]
#[ignore]
fn escape_the_group() {
    let Some(owner) = std::env::var(ESCAPE_OWNER)
        .ok()
        .and_then(|owner| owner.parse::<i32>().ok())
    else {
        return;
    };
    if std::env::var_os(ESCAPED).is_some() {
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        // SAFETY: signal 0 only checks that the process exists.
        while std::time::Instant::now() < deadline && unsafe { libc::kill(owner, 0) } == 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        return;
    }
    use std::os::unix::process::CommandExt as _;
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "daemon::oneshot::tests::escape_the_group",
            "--ignored",
        ])
        .env(ESCAPED, "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid_file = std::env::var_os(ESCAPE_PID_FILE).unwrap();
    fs::write(pid_file, child.id().to_string()).unwrap();
    // It outlives this process, which exits next, so nothing here waits.
    std::mem::forget(child);
}
