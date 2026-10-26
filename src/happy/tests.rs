use super::*;

fn strings(command: &[OsString]) -> Vec<&str> {
    command.iter().map(|s| s.to_str().unwrap()).collect()
}

#[test]
fn claude_sessions_take_the_prompt_after_happy_flags_and_before_arguments() {
    let command = command(
        Agent::Claude,
        Some("Fix #34"),
        vec!["--model".into(), "opus".into()],
    );
    assert_eq!(
        strings(&command),
        [
            "happy",
            "claude",
            "--happy-starting-mode",
            "remote",
            "--started-by",
            "daemon",
            "Fix #34",
            "--model",
            "opus",
        ]
    );
    let bare = super::command(Agent::Claude, None, vec![]);
    assert_eq!(bare.len(), 6);
    assert_eq!(bare[1], "claude");
}

#[test]
fn codex_sessions_never_receive_a_positional_prompt() {
    assert!(!accepts_prompt(Agent::Codex));
    let command = command(Agent::Codex, Some("Fix #34"), vec!["--yolo".into()]);
    assert_eq!(
        strings(&command),
        [
            "happy",
            "codex",
            "--happy-starting-mode",
            "remote",
            "--started-by",
            "daemon",
            "--yolo",
        ]
    );
}

#[test]
fn daemon_state_lives_in_the_happy_home() {
    let home = std::path::Path::new("/home/user");
    // The variable is process-wide; only the default is asserted here.
    if std::env::var_os(HOME_ENV).is_none() {
        assert_eq!(
            daemon_state_path(home),
            PathBuf::from("/home/user/.happy/daemon.state.json")
        );
    }
}
