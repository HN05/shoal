//! Restore commands omit the original task and remain available after wrapper exit.
use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::{
    cli::client,
    config::named_commands,
    fsutil::{Permissions, ReplaceOptions},
    model::Workspace,
    paths::Paths,
    protocol::ConfigTarget,
};

#[derive(Debug)]
pub(crate) struct Recovery {
    pub agent: String,
    pub command: Vec<OsString>,
    pub automatic: bool,
    /// Whether `command` carries the handoff message as the agent's prompt.
    pub handoff_delivered: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Record {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// The Herdr pane the agent ran in, where `shoal resume --all` restores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub herdr_pane: Option<String>,
}

impl Recovery {
    pub(super) async fn for_launch(paths: &Paths, workspace_id: &str, agent: &str) -> Self {
        let resolved = async {
            let workspace = client::inspect(paths, workspace_id.to_owned())
                .await?
                .workspace;
            Self::resolve(paths, &workspace, agent, None).await
        }
        .await;
        resolved.unwrap_or_else(|error| {
            eprintln!("warning: automatic agent recovery unavailable: {error:#}");
            Self {
                agent: agent.replace(' ', "-"),
                command: vec![],
                automatic: false,
                handoff_delivered: false,
            }
        })
    }

    /// `handoff` becomes the restored session's first prompt where the restore
    /// command accepts one: built-in agents and `{prompt}` in `[agent_resume]`.
    pub(crate) async fn resolve(
        paths: &Paths,
        workspace: &Workspace,
        agent: &str,
        handoff: Option<&str>,
    ) -> Result<Self> {
        let settings =
            client::settings(paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
        let name = agent.replace(' ', "-");
        let configured = settings.agent_resume.get(&name);
        let builtin = matches!(name.as_str(), "codex" | "claude");
        let automatic = configured.is_some() || builtin;
        let prompt = std::ffi::OsStr::new(handoff.unwrap_or_default());
        let (command, handoff_delivered) = if let Some(argv) = configured {
            let command = named_commands::expand_with_fields(
                paths,
                &settings.agent_resume,
                &name,
                workspace,
                vec![],
                &[("{prompt}", prompt)],
            )
            .await?;
            (command, argv.iter().any(|arg| arg.contains("{prompt}")))
        } else {
            let mut args: Vec<OsString> = match name.as_str() {
                // Codex's worktrees feature widens `--last` to every linked
                // worktree of the repository; restore this workspace's session.
                "codex" => ["resume", "--last", "-c", "features.worktrees=false"]
                    .map(OsString::from)
                    .into(),
                "claude" => vec!["--continue".into()],
                _ => vec![],
            };
            if let Ok(agent) = name.parse::<crate::agent::BuiltinAgent>() {
                args.extend(agent.message_hook_args()?);
            }
            if builtin {
                if handoff.is_some() {
                    args.push(prompt.to_owned());
                }
                let command =
                    named_commands::expand(paths, &settings.commands, &name, workspace, args)
                        .await?;
                (command, true)
            } else {
                (vec![], false)
            }
        };
        Ok(Self {
            agent: name,
            command,
            automatic,
            handoff_delivered: handoff_delivered && handoff.is_some(),
        })
    }

    pub(super) fn save(
        &self,
        paths: &Paths,
        workspace_id: &str,
        id: &str,
        reason: Option<&str>,
        herdr_pane: Option<String>,
    ) -> Result<PathBuf> {
        let record = Record::new(&self.agent, reason, herdr_pane);
        write_record(paths, workspace_id, id, record)
    }
}

impl Record {
    fn new(agent: &str, reason: Option<&str>, herdr_pane: Option<String>) -> Self {
        Self {
            agent: agent.replace(' ', "-"),
            stop_reason: reason.map(str::to_owned),
            herdr_pane,
        }
    }
}

/// Save the agent identity before an overload signal is delivered. This keeps
/// the handoff recoverable if the wrapper is terminated before it can write it.
pub(crate) fn save_record(
    paths: &Paths,
    workspace_id: &str,
    id: &str,
    agent: &str,
    reason: Option<&str>,
) -> Result<PathBuf> {
    write_record(paths, workspace_id, id, Record::new(agent, reason, None))
}

fn write_record(
    paths: &Paths,
    workspace_id: &str,
    id: &str,
    mut record: Record,
) -> Result<PathBuf> {
    let directory = paths.workspace_state(workspace_id);
    std::fs::create_dir_all(&directory)?;
    let path = record_path(paths, workspace_id, id);
    // A later save without a field, such as shutdown racing an overload stop
    // or the daemon's handoff after the wrapper's, keeps the value saved first.
    if let Some(saved) = std::fs::read(&path)
        .ok()
        .and_then(|saved| serde_json::from_slice::<Record>(&saved).ok())
    {
        record.stop_reason = record.stop_reason.or(saved.stop_reason);
        record.herdr_pane = record.herdr_pane.or(saved.herdr_pane);
    }
    crate::fsutil::replace_atomically(
        &path,
        &serde_json::to_vec(&record)?,
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: true,
        },
    )?;
    Ok(path)
}

pub(crate) fn record_path(paths: &Paths, workspace_id: &str, id: &str) -> PathBuf {
    paths
        .workspace_state(workspace_id)
        .join(format!("{id}{AGENT_SUFFIX}"))
}

const AGENT_SUFFIX: &str = ".recovery.json";
const COMMAND_SUFFIX: &str = ".command.json";

/// A command `shoal stop` interrupted, reported by `shoal resume` instead of
/// being rerun: replaying an arbitrary command is not known to be safe.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CommandRecord {
    pub argv: Vec<String>,
}

pub(super) fn save_command(
    paths: &Paths,
    workspace_id: &str,
    id: &str,
    argv: &[OsString],
) -> Result<PathBuf> {
    // A lossy record would report a different command than the one stopped.
    let record = CommandRecord {
        argv: argv
            .iter()
            .map(|arg| arg.to_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or_else(|| anyhow::anyhow!("an argument is not valid UTF-8"))?,
    };
    let directory = paths.workspace_state(workspace_id);
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{id}{COMMAND_SUFFIX}"));
    crate::fsutil::replace_atomically(
        &path,
        &serde_json::to_vec(&record)?,
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: true,
        },
    )?;
    Ok(path)
}

/// How a stopped agent's wait for automatic recovery ended.
#[derive(Debug)]
pub(super) enum Waited {
    Resume(Vec<crate::model::PortReservation>),
    /// The restore was cancelled, with the daemon's reason if it gave one.
    Cancelled(Option<String>),
}

pub(super) async fn wait(link: &mut super::Link) -> Result<Waited> {
    use crate::protocol::Control;
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut quit = signal(SignalKind::quit())?;
    let signalled = || Ok(Waited::Cancelled(Some("interrupted".into())));
    tokio::select! {
        control = link.control() => match control? {
            Control::Resume { ports } => Ok(Waited::Resume(ports)),
            Control::Stop { reason } => Ok(Waited::Cancelled(reason)),
            _ => anyhow::bail!("unexpected recovery control"),
        },
        _ = interrupt.recv() => signalled(),
        _ = terminate.recv() => signalled(),
        _ = quit.recv() => signalled(),
    }
}

/// What `shoal stop` or overload protection saved for one workspace.
#[derive(Debug, Default)]
pub(crate) struct Saved {
    /// Agent recovery records by execution ID.
    pub agents: Vec<(String, PathBuf)>,
    pub commands: Vec<SavedCommand>,
}

#[derive(Debug)]
pub(crate) struct SavedCommand {
    pub id: String,
    pub path: PathBuf,
    pub argv: Vec<String>,
}

impl Saved {
    pub fn load(paths: &Paths, workspace_id: &str) -> Result<Self> {
        let mut saved = Self::default();
        let entries = match std::fs::read_dir(paths.workspace_state(workspace_id)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(saved),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(id) = name.strip_suffix(AGENT_SUFFIX) {
                saved.agents.push((id.to_owned(), path));
            } else if let Some(id) = name.strip_suffix(COMMAND_SUFFIX) {
                let record: CommandRecord = serde_json::from_slice(&std::fs::read(&path)?)
                    .with_context(|| format!("read {}", path.display()))?;
                saved.commands.push(SavedCommand {
                    id: id.to_owned(),
                    path,
                    argv: record.argv,
                });
            }
        }
        saved.agents.sort();
        saved.commands.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(saved)
    }

    /// Keep only the record of execution `id`; a selected agent keeps the
    /// workspace's commands too when `with_commands`, so it receives them.
    pub fn retain_execution(&mut self, id: &str, with_commands: bool) {
        if self.agents.iter().any(|(agent, _)| agent == id) {
            self.agents.retain(|(agent, _)| agent == id);
            if !with_commands {
                self.commands.clear();
            }
        } else {
            self.agents.clear();
            self.commands.retain(|command| command.id == id);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.agents.is_empty() && self.commands.is_empty()
    }
}

/// Whether any workspace has stopped work; readable while the daemon is down.
pub fn any_pending(paths: &Paths) -> bool {
    let Ok(entries) = std::fs::read_dir(paths.state.join("workspaces")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|id| pending(paths, id).unwrap_or(false))
    })
}

/// Pending recovery is unfinished work even when its Git tree is clean.
pub fn pending(paths: &Paths, workspace_id: &str) -> Result<bool> {
    let entries = match std::fs::read_dir(paths.workspace_state(workspace_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(AGENT_SUFFIX) || name.ends_with(COMMAND_SUFFIX) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn consume(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_records_remain_readable_and_pressure_handoffs_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let old: Record = serde_json::from_str(r#"{"agent":"codex"}"#).unwrap();
        assert!(old.stop_reason.is_none());
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(root.path());
        let path = save_record(
            &paths,
            "workspace",
            "execution",
            "codex",
            Some("critical memory pressure"),
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let record: Record = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(record.agent, "codex");
        assert_eq!(
            record.stop_reason.as_deref(),
            Some("critical memory pressure")
        );
        assert!(pending(&paths, "workspace").unwrap());
        let recovery = Recovery {
            agent: "codex".into(),
            command: vec![],
            automatic: false,
            handoff_delivered: false,
        };
        recovery
            .save(&paths, "workspace", "execution", None, Some("w1:p2".into()))
            .unwrap();
        save_record(&paths, "workspace", "execution", "codex", None).unwrap();
        let record: Record = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            record.stop_reason.as_deref(),
            Some("critical memory pressure")
        );
        assert_eq!(record.herdr_pane.as_deref(), Some("w1:p2"));
    }

    #[test]
    fn commands_with_arguments_that_are_not_utf8_are_not_recorded() {
        use std::os::unix::ffi::OsStringExt;
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(root.path());
        let argv = [OsString::from("cat"), OsString::from_vec(vec![0xff])];
        assert!(save_command(&paths, "workspace", "execution", &argv).is_err());
        assert!(!pending(&paths, "workspace").unwrap());
    }
}
