use super::*;

fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", script])
        .env_clear()
        .env("SHOAL_TEST_PROCESS", "1");
    command
}

#[tokio::test]
async fn captures_literal_arguments_and_closes_stdin() {
    let mut command = shell("read value || printf '%s' \"$1\"");
    command.args(["--", "$(exit 9); literal"]);
    assert_eq!(
        Run::new(command).output().await.unwrap(),
        "$(exit 9); literal"
    );
}

#[tokio::test]
async fn input_and_output_can_exceed_pipe_buffers() {
    let input = vec![b'x'; 256 * 1024];
    let output = Run::new(shell("head -c 262144 /dev/zero; cat"))
        .input(input.clone())
        .timeout(Duration::from_secs(60))
        .checked()
        .await
        .unwrap();
    assert_eq!(&output.stdout[..input.len()], vec![0; input.len()]);
    assert_eq!(&output.stdout[input.len()..], input);
}

#[tokio::test]
async fn input_can_come_from_a_file() {
    let mut file = tempfile::tempfile().unwrap();
    std::io::Write::write_all(&mut file, b"from a file").unwrap();
    std::io::Seek::rewind(&mut file).unwrap();
    let output = Run::new(shell("cat"))
        .input_file(file)
        .output()
        .await
        .unwrap();
    assert_eq!(output, "from a file");
}

#[tokio::test]
async fn output_limits_cover_both_pipes_and_interrupt_nonterminating_children() {
    let output = Run::new(shell("printf 1234; printf 5678 >&2"))
        .output_limit(4)
        .capture()
        .await
        .unwrap();
    assert_eq!(output.stdout, b"1234");
    assert_eq!(output.stderr, b"5678");
    for redirect in ["", ">&2"] {
        let error = Run::new(shell(&format!("while :; do printf 12345 {redirect}; done")))
            .output_limit(4)
            .input(vec![0; 256 * 1024])
            .timeout(Duration::from_secs(60))
            .capture()
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
        assert!(
            error.to_string().contains("output exceeded 4 bytes"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn raw_status_and_non_utf8_stdout_are_preserved() {
    let output = Run::new(shell("printf '\\377'; exit 7"))
        .capture()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, [255]);
    assert!(
        Run::new(shell("printf '\\377'"))
            .output()
            .await
            .unwrap_err()
            .to_string()
            .contains("not UTF-8")
    );
}

#[tokio::test]
async fn reports_spawn_failure_and_limits_unicode_diagnostics() {
    let error = Run::new(Command::new("/nonexistent-shoal-test-tool"))
        .output()
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("/nonexistent-shoal-test-tool"));
    let text = format!("{}END", "é".repeat(MAX_DIAGNOSTIC_CHARS));
    let error = Run::new(shell("cat >&2; exit 1"))
        .input(text.into_bytes())
        .output()
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .ends_with(&"é".repeat(MAX_DIAGNOSTIC_CHARS))
    );
    assert!(!error.to_string().contains("END"));
}

#[tokio::test]
async fn early_stdin_close_preserves_exit_status_and_diagnostics() {
    let output = Run::new(shell("exec 0<&-; printf 'early exit' >&2; exit 7"))
        .input(vec![0; 1024 * 1024])
        .timeout(Duration::from_secs(60))
        .capture()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stderr, b"early exit");
    let error = checked_output("test tool", Ok(output)).unwrap_err();
    assert!(error.to_string().ends_with("early exit"));
    assert_eq!(
        Run::new(shell("exec 0<&-; printf done"))
            .input(vec![0; 1024 * 1024])
            .timeout(Duration::from_secs(60))
            .output()
            .await
            .unwrap(),
        "done"
    );
}

#[tokio::test]
async fn deadline_kills_child_even_when_stdin_is_blocked() {
    let root = tempfile::tempdir().unwrap();
    let pid_file = root.path().join("pid");
    let mut command = shell("echo $$ > \"$1\"; exec tail -f /dev/null");
    command.arg("--").arg(&pid_file).current_dir(root.path());
    let exchange = tokio::spawn(
        Run::new(command)
            .input(vec![0; 1024 * 1024])
            .timeout(Duration::from_secs(60))
            .capture(),
    );
    let pid: i32 = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = text.trim().parse()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child did not publish readiness");
    // Advance the deadline only once the nonterminating child has started.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    let error = exchange.await.unwrap().unwrap_err();
    tokio::time::resume();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    // The error names what hung and where.
    let message = error.to_string();
    assert!(message.starts_with("`/bin/sh -c echo"), "{message}");
    assert!(
        message.contains(&format!("in {}", root.path().display())),
        "{message}"
    );
    assert!(message.contains("within 60 seconds"), "{message}");
    tokio::time::timeout(Duration::from_secs(60), async {
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed-out child survived");
}

static OBSERVED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

#[tokio::test]
async fn observer_receives_each_run_with_long_arguments_cut_to_their_end() {
    observe(|command, _| OBSERVED.lock().unwrap().push(command.to_owned()));
    let long = format!("{}{}", "x".repeat(10), "y".repeat(OBSERVED_ARGUMENT_CHARS));
    let mut command = shell("exit 0");
    command.args(["observed-run", &long]);
    Run::new(command).capture().await.unwrap();
    let cut = format!("…{}", "y".repeat(OBSERVED_ARGUMENT_CHARS - 1));
    let expected = format!("/bin/sh -c exit 0 observed-run {cut}");
    assert!(OBSERVED.lock().unwrap().contains(&expected));
}
