use super::*;

#[test]
fn the_typed_line_stays_out_of_history_and_keeps_the_directive_in_the_shell() {
    let launch = [
        "/opt/shoal".to_owned(),
        "internal".into(),
        "launch".into(),
        "/tmp/f".into(),
    ];
    assert_eq!(
        typed_text(&shell_line(&launch, None), false),
        " /opt/shoal internal launch /tmp/f\r"
    );
    assert_eq!(
        typed_text(&shell_line(&launch, Some("/tmp/a b")), false),
        " export SHOAL_SHELL_DIRECTIVE='/tmp/a b'; /opt/shoal internal launch /tmp/f\r"
    );
}

#[test]
fn a_place_command_runs_the_launch_in_the_foreground_then_the_login_shell() {
    let line = shell_line(
        &["/opt/shoal".to_owned(), "launch".into()],
        Some("/tmp/a b"),
    );
    assert_eq!(
        place_command(&line, &[], false),
        [
            "/bin/sh",
            "-c",
            "set -m; export SHOAL_SHELL_DIRECTIVE='/tmp/a b'; /opt/shoal launch; exec \"${SHELL:-/bin/sh}\" -l"
        ]
    );
    assert_eq!(
        place_command(&line, &[], true)[2],
        format!("set -m; {line}")
    );
}

#[test]
fn a_place_command_runs_the_launch_and_the_shell_without_cleared_variables() {
    assert_eq!(
        place_command("/opt/shoal launch", &["HERDR_ENV", "ZMX_SESSION"], false)[2],
        "set -m; unset HERDR_ENV ZMX_SESSION; /opt/shoal launch; exec \"${SHELL:-/bin/sh}\" -l"
    );
}

#[test]
fn the_launch_file_carries_the_command_and_goes_once_read() {
    let typed = Typed::new(
        Path::new("/state"),
        &["/opt/shoal".into(), "a b".into()],
        &[("SHOAL_ADD_PLAN", "{\"secret\":1}".into())],
    )
    .unwrap();
    assert!(!typed.text().contains("secret"));
    assert!(!typed.command().concat().contains("secret"));
    let file = typed.file.to_path_buf();
    typed.hand_over().unwrap();
    let launch = read(&file).unwrap();
    assert_eq!(launch.argv, ["/opt/shoal", "a b"]);
    assert_eq!(launch.env["SHOAL_ADD_PLAN"], "{\"secret\":1}");
    assert!(!file.exists());
}

#[test]
fn the_launch_file_names_what_the_command_runs_without() {
    let typed = Typed::clearing(
        Path::new("/state"),
        &["/opt/shoal".into()],
        &[],
        &["HERDR_ENV"],
        true,
    )
    .unwrap();
    assert!(!typed.command()[2].contains("exec"));
    let file = typed.file.to_path_buf();
    typed.hand_over().unwrap();
    assert_eq!(read(&file).unwrap().clear, ["HERDR_ENV"]);
}

#[test]
fn a_launch_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let typed = Typed::new(Path::new("/state"), &["/opt/shoal".into()], &[]).unwrap();
    let mode = std::fs::metadata(&typed.file).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0);
}

#[test]
fn the_launch_step_names_the_state_directory_and_output_as_workers_do() {
    let launch = launch_argv(Path::new("/state dir"), Path::new("/tmp/f")).unwrap();
    assert_eq!(
        &launch[1..],
        [
            "--state-dir",
            "/state dir",
            "--text",
            "internal",
            "launch",
            "/tmp/f"
        ]
    );
}

#[test]
fn a_command_that_ends_its_shell_exits_after_it() {
    let launch = ["/opt/shoal".to_owned(), "launch".into()];
    assert_eq!(
        typed_text(&shell_line(&launch, None), true),
        " /opt/shoal launch; exit\r"
    );
}

#[tokio::test(start_paused = true)]
async fn typing_waits_for_the_line_editor() {
    let mut reads = 0;
    await_prompt(|| {
        reads += 1;
        std::future::ready(Some(reads < 3))
    })
    .await;
    assert_eq!(reads, 3);
}

#[tokio::test(start_paused = true)]
async fn a_shell_without_a_line_editor_is_typed_into_after_the_limit() {
    let start = tokio::time::Instant::now();
    await_prompt(|| std::future::ready(Some(true))).await;
    assert!(start.elapsed() >= PROMPT_LIMIT);
}

#[tokio::test(start_paused = true)]
async fn a_read_that_never_answers_ends_at_the_limit() {
    let start = tokio::time::Instant::now();
    await_prompt(std::future::pending::<Option<bool>>).await;
    assert!(start.elapsed() >= PROMPT_LIMIT);
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_terminal_is_not_waited_for() {
    let mut reads = 0;
    await_prompt(|| {
        reads += 1;
        std::future::ready(None)
    })
    .await;
    assert_eq!(reads, 1);
}
