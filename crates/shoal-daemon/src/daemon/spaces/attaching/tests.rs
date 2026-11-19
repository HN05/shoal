use std::ffi::OsStr;

use super::*;

#[tokio::test]
async fn the_attach_runs_with_the_viewer_s_term_on_no_host_s_terminal() {
    let (_pty, terminal) = pty::open(None).unwrap();
    let viewer = Viewer {
        term: Some("xterm-ghostty".into()),
        ..Viewer::default()
    };
    let command = attach_command(&["shoal".into()], &viewer, terminal, REPORT).unwrap();
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
