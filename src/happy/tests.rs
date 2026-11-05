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
    let words = strings(&command);
    assert_eq!(words[..2], ["happy", "claude"]);
    assert_eq!(words[words.len() - 3..], ["Fix #34", "--model", "opus"]);
    let bare = super::command(Agent::Claude, None, vec![]);
    assert_eq!(strings(&bare), words[..words.len() - 3]);
}

#[test]
fn codex_sessions_never_receive_a_positional_prompt() {
    let command = command(Agent::Codex, Some("Fix #34"), vec!["--yolo".into()]);
    let words = strings(&command);
    assert_eq!(words[..2], ["happy", "codex"]);
    assert_eq!(words.last(), Some(&"--yolo"));
    assert!(!words.contains(&"Fix #34"));
}
