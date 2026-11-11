use super::*;

#[test]
fn elapsed_times_read_as_ps_writes_them() {
    assert_eq!(elapsed("00:07"), Some(7));
    assert_eq!(elapsed("41:00"), Some(41 * 60));
    assert_eq!(elapsed("02:03:04"), Some(2 * 3600 + 3 * 60 + 4));
    assert_eq!(elapsed("3-00:00:01"), Some(3 * 86_400 + 1));
    assert_eq!(elapsed("soon"), None);
}

#[test]
fn a_line_keeps_the_whole_command() {
    assert_eq!(
        parse_line("  4121   41:00 target/debug/shoal run -- sh -c 'sleep 1'"),
        Some((
            4121,
            Description {
                command: "target/debug/shoal run -- sh -c 'sleep 1'".into(),
                elapsed: 41 * 60,
            }
        ))
    );
}

#[tokio::test]
async fn running_processes_are_described_and_exited_ones_left_out() {
    let mut child = Command::new("sleep")
        .arg("300")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let running = child.id().unwrap();
    let mut exited = Command::new("true").spawn().unwrap();
    let gone = exited.id().unwrap();
    exited.wait().await.unwrap();

    let described = describe(&[running, gone]).await.unwrap();

    assert_eq!(described[&running].command, "sleep 300");
    assert!(!described.contains_key(&gone));
    child.kill().await.unwrap();
}
