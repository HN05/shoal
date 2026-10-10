//! zmx sessions keep terminal agents running after their terminal closes. zmx
//! owns the pty, passes output through unchanged while attached, and restores
//! the screen on attach; Shoal names each session after its workspace and
//! labels it with the workspace ID so `shoal attach` can find it.
use std::{
    collections::HashSet, ffi::OsString, io::IsTerminal, path::PathBuf, process::ExitStatus,
};

use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use tokio::process::Command;

use super::{
    agents::ensure_program,
    context::Context,
    herdr,
    internal::{Worker, internal_command},
};
use crate::{config::Effective, env, model::Workspace, subprocess::Run, tools::Tool};

/// zmx sets this inside a session; a launch there runs in place.
const SESSION_VAR: &str = "ZMX_SESSION";
const WORKSPACE_LABEL: &str = "shoal.workspace";
const AGENT_LABEL: &str = "shoal.agent";
/// zmx names its socket after the session, and `sun_path` is short on macOS.
const MAX_NAME_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub name: String,
    pub agent: Option<String>,
    /// Terminals attached to the session.
    pub clients: u32,
}

impl Session {
    /// The name, agent, and whether a terminal is attached.
    pub fn label(&self) -> String {
        let mut label = self.name.clone();
        if let Some(agent) = &self.agent {
            label.push_str(&format!("  {agent}"));
        }
        if self.clients > 0 {
            label.push_str("  attached");
        }
        label
    }
}

/// Whether a terminal agent launch runs in a zmx session: it needs a terminal
/// to attach, never nests, and leaves an enabled Herdr pane to Herdr, which
/// reads the agent's state from the process in the pane.
pub(in crate::cli) fn hosts(settings: &Effective) -> bool {
    on_terminal() && !(settings.herdr.enabled && herdr::current_pane().is_some())
}

/// Whether this process could attach a new session: a terminal outside zmx.
pub(in crate::cli) fn on_terminal() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::env::var_os(SESSION_VAR).is_none()
}

/// Start `command` as `agent`'s tracked execution in a new session and attach
/// this terminal to it. zmx does not report the agent's exit status, so a
/// session that ends or detaches returns 0; the daemon still records the exit.
pub(in crate::cli) async fn run(
    ctx: &Context,
    workspace: &Workspace,
    agent: &str,
    records: &[PathBuf],
    command: &[OsString],
) -> Result<i32> {
    // Launches check zmx before any work; resumed agents reach it here.
    ensure_program(agent, Tool::Zmx.program())?;
    let taken = list().await?.into_iter().map(|session| session.0).collect();
    let name = free_name(&workspace.name, &taken);
    let worker = internal_command(
        &ctx.paths,
        false,
        Worker::Session {
            workspace: &workspace.id,
            agent,
            records,
            command,
        },
    )?;
    let mut zmx = Command::new(Tool::Zmx.program());
    zmx.arg("attach")
        .arg("--labels")
        .arg(labels(&workspace.id, agent))
        .arg(&name)
        .args(&worker)
        .env_remove(env::SHELL_DIRECTIVE);
    let status = zmx.status().await.context("run zmx")?;
    finish(workspace, &name, status).await
}

/// Attach this terminal to `session`, reporting a detach as `run` does.
pub(in crate::cli) async fn attach(workspace: &Workspace, session: &str) -> Result<i32> {
    ensure_program("attach", Tool::Zmx.program())?;
    let status = Command::new(Tool::Zmx.program())
        .args(["attach", session])
        .status()
        .await
        .context("run zmx")?;
    finish(workspace, session, status).await
}

async fn finish(workspace: &Workspace, name: &str, status: ExitStatus) -> Result<i32> {
    if !status.success() {
        bail!("zmx exited with {status}");
    }
    if sessions(&workspace.id)
        .await?
        .iter()
        .any(|session| session.name == name)
    {
        eprintln!(
            "shoal: detached from {name}; run `shoal attach {}` to return",
            workspace.name
        );
    }
    Ok(0)
}

/// The workspace's running sessions, oldest name first.
pub async fn sessions(workspace_id: &str) -> Result<Vec<Session>> {
    let mut sessions: Vec<_> = list()
        .await?
        .into_iter()
        .filter(|(_, fields)| field(fields, WORKSPACE_LABEL) == Some(workspace_id))
        .map(|(name, fields)| Session {
            agent: field(&fields, AGENT_LABEL).map(str::to_owned),
            clients: field(&fields, "clients")
                .and_then(|clients| clients.parse().ok())
                .unwrap_or(0),
            name,
        })
        .collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sessions)
}

type Fields = Vec<(String, String)>;

/// Every zmx session with its fields and labels.
async fn list() -> Result<Vec<(String, Fields)>> {
    let mut command = Command::new(Tool::Zmx.program());
    command.arg("list");
    let output = Run::new(command)
        .checked()
        .await
        .context("list zmx sessions")?;
    Ok(parse_list(&String::from_utf8_lossy(&output.stdout)))
}

/// `zmx list` prints one session per line after a two-column marker, as
/// tab-separated `key=value` fields followed by its labels.
fn parse_list(output: &str) -> Vec<(String, Fields)> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Fields = line[line.find("name=")?..]
                .split('\t')
                .filter_map(|field| field.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect();
            let name = field(&fields, "name")?.to_owned();
            Some((name, fields))
        })
        .collect()
}

fn field<'a>(fields: &'a Fields, key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// zmx separates labels with spaces, so an agent name containing one is left out.
fn labels(workspace_id: &str, agent: &str) -> String {
    let mut labels = format!("{WORKSPACE_LABEL}={workspace_id}");
    if !agent.is_empty() && !agent.contains(char::is_whitespace) {
        labels.push_str(&format!(" {AGENT_LABEL}={agent}"));
    }
    labels
}

/// The workspace name, shortened to fit a socket path, then suffixed `-2`,
/// `-3`, ... past sessions that already use it.
fn free_name(workspace: &str, taken: &HashSet<String>) -> String {
    let base: String = workspace.chars().take(MAX_NAME_LEN).collect();
    std::iter::once(base.clone())
        .chain((2..).map(|n| format!("{base}-{n}")))
        .find(|name| !taken.contains(name))
        .expect("an unused suffix exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_parses_names_fields_and_labels_after_the_marker() {
        let output = "  name=fix-login\tpid=1\tclients=0\tcreated=1\tcwd=file://host/a\t\
            cmd=shoal internal session\tshoal.agent=claude\tshoal.workspace=w1\n\
            \u{2192} name=other\tpid=2\tclients=1\tcmd=sh -c 'x=y'\n\
            no sessions found in /tmp/zmx-501\n";
        let sessions = parse_list(output);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].0, "fix-login");
        assert_eq!(field(&sessions[0].1, "shoal.workspace"), Some("w1"));
        assert_eq!(field(&sessions[0].1, "shoal.agent"), Some("claude"));
        assert_eq!(sessions[1].0, "other");
        assert_eq!(field(&sessions[1].1, "clients"), Some("1"));
        assert_eq!(field(&sessions[1].1, "cmd"), Some("sh -c 'x=y'"));
    }

    #[test]
    fn names_fit_the_socket_path_and_skip_taken_ones() {
        let taken = HashSet::from(["fix".to_owned(), "fix-2".to_owned()]);
        assert_eq!(free_name("fix", &taken), "fix-3");
        assert_eq!(free_name("other", &taken), "other");
        assert_eq!(free_name(&"x".repeat(40), &taken), "x".repeat(MAX_NAME_LEN));
    }

    #[test]
    fn labels_leave_out_agent_names_zmx_cannot_hold() {
        assert_eq!(
            labels("w1", "claude"),
            "shoal.workspace=w1 shoal.agent=claude"
        );
        assert_eq!(labels("w1", "my agent"), "shoal.workspace=w1");
    }
}
