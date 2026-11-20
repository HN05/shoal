use std::ffi::OsStr;

use super::*;

#[tokio::test]
async fn the_attach_runs_with_the_viewer_s_term_on_no_host_s_terminal() {
    let (_pty, terminal) = pty::open(None).unwrap();
    let command = attach_command(&["shoal".into()], Some("xterm-ghostty"), terminal, REPORT);
    let command = command.unwrap();
    let envs: Vec<_> = command.as_std().get_envs().collect();
    assert!(envs.contains(&(OsStr::new("TERM"), Some(OsStr::new("xterm-ghostty")))));
    // A daemon started inside tmux would otherwise switch its own client.
    for inherited in ["TMUX", "TMUX_PANE", "ZELLIJ", "HERDR_PANE_ID"] {
        assert!(
            envs.contains(&(OsStr::new(inherited), None)),
            "{inherited}: {envs:?}"
        );
    }
}

#[tokio::test]
async fn the_viewer_hears_a_report_the_attach_writes_after_its_terminal_closes() -> Result<()> {
    let left = serde_json::to_string(&AttachReport::Left { open: true })?;
    // The terminal closes well before the report comes, as when the
    // attach's last step outlasts the host's client.
    let script = "exec 0<&- 1>&- 2>&-; sleep 0.3; printf '%s\\n' \"$1\" >&3";
    let argv = ["sh", "-c", script, "sh", &left].map(std::ffi::OsString::from);
    let home = tempfile::tempdir()?;
    let mut terminal = ViewerTerminal::start(&argv, &Viewer::default(), home.path())?;
    let (viewer, relayed) = UnixStream::pair()?;
    let mut input = ViewerInput::start(terminal.pty.clone(), relayed.into_split().0);
    let mut shown = Vec::new();
    let status = loop {
        match terminal.next(&mut input).await? {
            Some(Step::Show(frame)) => shown.push(frame),
            Some(Step::Ended(status)) => break status,
            None => anyhow::bail!("the viewer went"),
        }
    };
    input.stop().await;
    drop(viewer);
    assert!(status.success(), "{status}");
    assert!(
        (shown.iter()).any(|frame| matches!(frame, FromSession::Left(true))),
        "{shown:?}"
    );
    Ok(())
}
