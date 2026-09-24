//! Happy (`happy-coder`) session launches. Happy's daemon starts app-visible
//! sessions as `happy <agent> --happy-starting-mode remote --started-by
//! daemon`; Shoal runs the same command detached in a workspace so the session
//! appears in the Happy app and stays a tracked execution. `client` seeds a
//! session on Happy's server so agents without a prompt argument still get one.
pub mod client;
mod crypto;

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::state::states;

/// Happy's home directory override and the daemon state file inside it.
pub const HOME_ENV: &str = "HAPPY_HOME_DIR";
pub const DAEMON_STATE_FILE: &str = "daemon.state.json";

/// Where Happy keeps its state: `$HAPPY_HOME_DIR` or `~/.happy`.
pub fn home(user_home: &Path) -> PathBuf {
    std::env::var_os(HOME_ENV)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| user_home.join(".happy"))
}

states!(HappyAgent: ValueEnum {
    Claude => "claude",
    Codex => "codex",
});

impl HappyAgent {
    pub fn name(self) -> &'static str {
        self.as_str()
    }

    /// `happy claude` forwards unknown arguments to Claude Code, so a
    /// positional prompt reaches it. `happy codex` parses only its own flags
    /// and ignores everything else, so a prompt cannot be delivered that way.
    pub fn accepts_prompt(self) -> bool {
        self == HappyAgent::Claude
    }
}

/// The session command: Happy's own flags first, in the order its daemon
/// uses, then the prompt (when the agent can take one) and caller arguments.
pub fn command(agent: HappyAgent, prompt: Option<&str>, args: Vec<OsString>) -> Vec<OsString> {
    let mut command: Vec<OsString> = vec![
        "happy".into(),
        agent.name().into(),
        "--happy-starting-mode".into(),
        "remote".into(),
        "--started-by".into(),
        "daemon".into(),
    ];
    if let Some(prompt) = prompt.filter(|_| agent.accepts_prompt()) {
        command.push(prompt.into());
    }
    command.extend(args);
    command
}

/// Where Happy's daemon records itself.
pub fn daemon_state_path(user_home: &Path) -> PathBuf {
    home(user_home).join(DAEMON_STATE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(command: &[OsString]) -> Vec<&str> {
        command.iter().map(|s| s.to_str().unwrap()).collect()
    }

    #[test]
    fn claude_sessions_take_the_prompt_after_happy_flags_and_before_arguments() {
        let command = command(
            HappyAgent::Claude,
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
        let bare = super::command(HappyAgent::Claude, None, vec![]);
        assert_eq!(bare.len(), 6);
        assert_eq!(bare[1], "claude");
    }

    #[test]
    fn codex_sessions_never_receive_a_positional_prompt() {
        assert!(!HappyAgent::Codex.accepts_prompt());
        let command = command(HappyAgent::Codex, Some("Fix #34"), vec!["--yolo".into()]);
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
}
