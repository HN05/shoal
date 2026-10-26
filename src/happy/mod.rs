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

states!(
    /// The agents Happy runs.
    Agent: ValueEnum {
        Claude => "claude",
        Codex => "codex",
    }
);

impl std::str::FromStr for Agent {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|agent| agent.as_str() == value)
            .ok_or(())
    }
}

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

/// `happy claude` forwards unknown arguments to Claude Code, so a
/// positional prompt reaches it. `happy codex` parses only its own flags
/// and ignores everything else, so a prompt cannot be delivered that way.
pub fn accepts_prompt(agent: Agent) -> bool {
    agent == Agent::Claude
}

/// The session command: Happy's own flags first, in the order its daemon
/// uses, then the prompt (when the agent can take one) and caller arguments.
pub fn command(agent: Agent, prompt: Option<&str>, args: Vec<OsString>) -> Vec<OsString> {
    let mut command: Vec<OsString> = vec![
        "happy".into(),
        agent.as_str().into(),
        "--happy-starting-mode".into(),
        "remote".into(),
        "--started-by".into(),
        "daemon".into(),
    ];
    if let Some(prompt) = prompt.filter(|_| accepts_prompt(agent)) {
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
mod tests;
