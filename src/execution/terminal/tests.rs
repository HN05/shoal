use super::*;

#[tokio::test]
async fn a_command_takes_the_terminal_as_it_starts() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let output = crate::test_support::in_new_terminal(
        root.path(),
        "execution::terminal::tests::foreground_child",
    )
    .await;
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test]
#[ignore = "isolated terminal owner launched by handoff regression"]
async fn foreground_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let terminal = Terminal::capture(true).unwrap().unwrap();
    let mut command = Command::new("perl");
    command.args([
        "-MPOSIX",
        "-e",
        "exit(POSIX::tcgetpgrp(0) == getpgrp() ? 0 : 3)",
    ]);
    terminal.hand_over_at_start(&mut command);
    // Nothing gives the command the terminal but its own start.
    let status = command
        .process_group(0)
        .spawn()
        .unwrap()
        .wait()
        .await
        .unwrap();
    drop(terminal);
    assert_eq!(status.code(), Some(0));
}
