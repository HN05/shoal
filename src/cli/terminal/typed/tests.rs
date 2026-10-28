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
        typed_text(&launch, None),
        " /opt/shoal internal launch /tmp/f\r"
    );
    assert_eq!(
        typed_text(&launch, Some("/tmp/a b")),
        " export SHOAL_SHELL_DIRECTIVE='/tmp/a b'; /opt/shoal internal launch /tmp/f\r"
    );
}

#[test]
fn the_launch_file_carries_the_command_and_goes_once_read() {
    let typed = Typed::new(
        &["/opt/shoal".into(), "a b".into()],
        &[("SHOAL_ADD_PLAN", "{\"secret\":1}".into())],
    )
    .unwrap();
    assert!(!typed.text().contains("secret"));
    let file = typed.file.to_path_buf();
    typed.hand_over().unwrap();
    let launch = read(&file).unwrap();
    assert_eq!(launch.argv, ["/opt/shoal", "a b"]);
    assert_eq!(launch.env["SHOAL_ADD_PLAN"], "{\"secret\":1}");
    assert!(!file.exists());
}

#[test]
fn a_launch_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let typed = Typed::new(&["/opt/shoal".into()], &[]).unwrap();
    let mode = std::fs::metadata(&typed.file).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0);
}
