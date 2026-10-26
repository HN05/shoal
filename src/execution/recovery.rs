//! Restore commands omit the original task and remain available after wrapper exit.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

mod claim;

pub(crate) use claim::{RecoveryClaim, claim, discard_saved};

use crate::{
    cli::{client, commands::configured},
    fsutil::{Permissions, ReplaceOptions},
    model::Workspace,
    paths::Paths,
    process::identity::{self, Identity},
    protocol::ConfigTarget,
    state::states,
};

#[derive(Debug)]
pub(crate) struct Recovery {
    pub agent: String,
    pub command: Vec<OsString>,
    pub automatic: bool,
    /// Whether `command` carries the handoff message as the agent's prompt.
    pub handoff_delivered: bool,
}

states!(
    /// How far a stop has progressed, so reconciliation after a daemon
    /// interruption knows which wrapper transition was still expected.
    Stage {
        /// The daemon decided to stop the agent and expects its wrapper to stop it.
        Stopping => "stopping",
        /// The wrapper stopped the agent, then pauses for an automatic restore
        /// or exits.
        Stopped => "stopped",
    }
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Unknown for records saved before stages existed, which may come from
    /// either side of the stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<Stage>,
    /// Where the agent ran, as its terminal backend names it, so `shoal
    /// resume --all` can restore it there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// The wrapper that restores the agent automatically while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<Identity>,
    /// The session the agent's hooks last reported, which built-in agents
    /// resume by ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The workspace moved where the agent's session is not available, such
    /// as from another machine, so the agent starts a new one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moved: bool,
}

/// What a new agent of a workspace that moved where its earlier session is
/// not available is told, followed by `handoff`.
pub(crate) fn moved_prompt(handoff: Option<&str>) -> String {
    let mut prompt = String::from(
        "Shoal moved this workspace, and your earlier session is not available here. Continue working on the unfinished assignment: read its linked issue and PRs, its commits and its uncommitted changes to see where it stands.",
    );
    if let Some(handoff) = handoff {
        prompt.push_str("\n\n");
        prompt.push_str(handoff);
    }
    prompt
}

impl Recovery {
    pub(super) async fn for_launch(paths: &Paths, workspace_id: &str, agent: &str) -> Self {
        let resolved = async {
            let workspace = client::inspect(paths, workspace_id.to_owned())
                .await?
                .workspace;
            Self::resolve(paths, &workspace, agent, None, None).await
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

    /// Ask the restored session to continue, including `handoff` where the
    /// command accepts a prompt: agents with resume arguments and `{prompt}`
    /// in `[agent_resume]`. Resume arguments restore `session` when known,
    /// otherwise the workspace's latest session.
    pub(crate) async fn resolve(
        paths: &Paths,
        workspace: &Workspace,
        agent: &str,
        handoff: Option<&str>,
        session: Option<&str>,
    ) -> Result<Self> {
        let settings =
            client::settings(paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
        let name = agent.replace(' ', "-");
        let configured = settings.agent_resume.get(&name);
        let resume_args = settings.launch(&name).arguments.resume_args(session);
        let automatic = configured.is_some() || resume_args.is_some();
        let mut message = String::from(
            "Shoal resumed this session. Continue working on the unfinished assignment from where you left off.",
        );
        if let Some(handoff) = handoff {
            message.push_str("\n\n");
            message.push_str(handoff);
        }
        let prompt = std::ffi::OsStr::new(&message);
        let (command, handoff_delivered) = if let Some(argv) = configured {
            let command = configured::expand_with_fields(
                paths,
                &settings.agent_resume,
                &name,
                workspace,
                vec![],
                &[("{prompt}", prompt)],
            )
            .await?;
            (command, argv.iter().any(|arg| arg.contains("{prompt}")))
        } else if let Some(mut args) = resume_args {
            args.extend(
                crate::cli::agents::workspace_args(paths, &settings, &name, workspace).await?,
            );
            let takes_field = settings
                .commands
                .get(&name)
                .is_some_and(|argv| argv.iter().any(|arg| arg.contains("{prompt}")));
            if !takes_field {
                args.push(prompt.to_owned());
            }
            let command = configured::expand_with_fields(
                paths,
                &settings.commands,
                &name,
                workspace,
                args,
                &[("{prompt}", prompt)],
            )
            .await?;
            (command, true)
        } else {
            (vec![], false)
        };
        Ok(Self {
            agent: name,
            command,
            automatic,
            handoff_delivered: handoff_delivered && handoff.is_some(),
        })
    }

    /// The command restoring the agent stopped with `record`: the session it
    /// saved, otherwise the restore command resolved at launch.
    pub(super) async fn restore_command(
        &self,
        paths: &Paths,
        workspace: &Workspace,
        record: Option<&Path>,
    ) -> Vec<OsString> {
        let session = record
            .and_then(|path| Record::load(path).ok().flatten())
            .and_then(|record| record.session);
        let Some(session) = session else {
            return self.command.clone();
        };
        match Self::resolve(paths, workspace, &self.agent, None, Some(&session)).await {
            Ok(recovery) if !recovery.command.is_empty() => recovery.command,
            Ok(_) => self.command.clone(),
            Err(error) => {
                eprintln!("warning: cannot resume session {session}: {error:#}");
                self.command.clone()
            }
        }
    }

    /// `waiting` names this wrapper when it restores the agent automatically.
    pub(super) fn save(
        &self,
        paths: &Paths,
        workspace_id: &str,
        id: &str,
        reason: Option<&str>,
        place: Option<String>,
        waiting: Option<Identity>,
    ) -> Result<PathBuf> {
        let mut record = Record::new(&self.agent, reason, Stage::Stopped, place);
        record.waiting = waiting;
        write_record(paths, workspace_id, id, record)
    }
}

impl Record {
    fn new(agent: &str, reason: Option<&str>, stage: Stage, place: Option<String>) -> Self {
        Self {
            agent: agent.replace(' ', "-"),
            stop_reason: reason.map(str::to_owned),
            stage: Some(stage),
            place,
            waiting: None,
            session: None,
            moved: false,
        }
    }

    /// This record where neither its session nor its place exists, such as
    /// on another machine, and no wrapper waits to restore it.
    pub(crate) fn moved(self) -> Self {
        Self {
            session: None,
            moved: true,
            ..self.elsewhere()
        }
    }

    /// This record on another machine, where its place does not exist and no
    /// wrapper waits to restore it, but its session may.
    pub(crate) fn elsewhere(self) -> Self {
        Self {
            place: None,
            waiting: None,
            ..self
        }
    }

    /// Read a saved record, `None` once it is consumed.
    pub(crate) fn load(path: &Path) -> Result<Option<Self>> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .with_context(|| format!("read {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
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
    let record = Record::new(agent, reason, Stage::Stopping, None);
    write_record(paths, workspace_id, id, record)
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
    record.session = record
        .session
        .or_else(|| reported_session(paths, workspace_id, id));
    // A later save without a field, such as shutdown racing an overload stop
    // or the daemon's handoff after the wrapper's, keeps the value saved first,
    // and never moves the stage back.
    if let Ok(Some(saved)) = Record::load(&path) {
        record.stop_reason = record.stop_reason.or(saved.stop_reason);
        record.place = record.place.or(saved.place);
        record.waiting = record.waiting.or(saved.waiting);
        record.session = record.session.or(saved.session);
        if saved.stage == Some(Stage::Stopped) {
            record.stage = saved.stage;
        }
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

/// An agent stopped in a removed workspace, which `shoal work restore` saves
/// again for `shoal resume`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoppedAgent {
    pub execution: String,
    pub record: Record,
    /// When its record was saved, in Unix seconds, which `shoal resume`
    /// shows as its stop time.
    pub stopped_at: u64,
}

/// The workspace's stopped agents, for its removal record. Their wrappers have
/// exited, so none waits to restore them.
pub(crate) fn stopped_agents(paths: &Paths, workspace_id: &str) -> Result<Vec<StoppedAgent>> {
    let mut agents = Vec::new();
    for (execution, path) in Saved::load(paths, workspace_id)?.agents {
        if let Some(mut record) = Record::load(&path)? {
            record.waiting = None;
            let stopped_at = std::fs::metadata(&path)?
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            agents.push(StoppedAgent {
                execution,
                record,
                stopped_at,
            });
        }
    }
    Ok(agents)
}

/// Save a removed workspace's stopped agents for the workspace restoring it.
pub(crate) fn save_stopped_agents(
    paths: &Paths,
    workspace_id: &str,
    agents: &[StoppedAgent],
) -> Result<()> {
    for agent in agents {
        let path = write_record(paths, workspace_id, &agent.execution, agent.record.clone())?;
        let stopped = std::time::UNIX_EPOCH + std::time::Duration::from_secs(agent.stopped_at);
        std::fs::File::open(path)?.set_modified(stopped)?;
    }
    Ok(())
}

/// Remove a record whose stop never reached the agent, returning whether it
/// did. A record the wrapper saved after stopping the agent is kept.
pub(crate) fn discard_undelivered(paths: &Paths, workspace_id: &str, id: &str) -> Result<bool> {
    let path = record_path(paths, workspace_id, id);
    match Record::load(&path)? {
        Some(record) if record.stage == Some(Stage::Stopping) => {
            consume(&path)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) fn record_path(paths: &Paths, workspace_id: &str, id: &str) -> PathBuf {
    paths
        .workspace_state(workspace_id)
        .join(format!("{id}{AGENT_SUFFIX}"))
}

const AGENT_SUFFIX: &str = ".recovery.json";
const COMMAND_SUFFIX: &str = ".command.json";
const SESSION_SUFFIX: &str = ".session";
const LAST_SESSION: &str = "last-session.json";

/// Whether `session` can name an agent session on a restore command line:
/// letters, digits, `-` and `_`, never read as an option.
pub(crate) fn valid_session(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && !session.starts_with('-')
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn session_path(paths: &Paths, workspace_id: &str, id: &str) -> PathBuf {
    paths
        .workspace_state(workspace_id)
        .join(format!("{id}{SESSION_SUFFIX}"))
}

/// Record the session the hooks of execution `id` report, writing only a change.
pub(crate) fn save_session(
    paths: &Paths,
    workspace_id: &str,
    id: &str,
    session: &str,
) -> Result<()> {
    if reported_session(paths, workspace_id, id).as_deref() == Some(session) {
        return Ok(());
    }
    std::fs::create_dir_all(paths.workspace_state(workspace_id))?;
    crate::fsutil::replace_atomically(
        &session_path(paths, workspace_id, id),
        session.as_bytes(),
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: false,
        },
    )?;
    Ok(())
}

/// The session the hooks of execution `id` last reported.
fn reported_session(paths: &Paths, workspace_id: &str, id: &str) -> Option<String> {
    std::fs::read_to_string(session_path(paths, workspace_id, id))
        .ok()
        .filter(|session| valid_session(session))
}

/// Forget the session of an execution that has ended.
pub(crate) fn forget_session(paths: &Paths, workspace_id: &str, id: &str) -> Result<()> {
    consume(&session_path(paths, workspace_id, id))
}

/// The workspace's last agent that exited on its own, which `shoal resume`
/// continues when nothing is stopped there. Unlike stopped work, it uses no
/// agent slot and never delays cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSession {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The workspace moved where the session is not available.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moved: bool,
}

impl LastSession {
    /// This session where it is not available, which starts a new one.
    pub(crate) fn moved(self) -> Self {
        Self {
            session: None,
            moved: true,
            ..self
        }
    }

    pub(crate) fn path(paths: &Paths, workspace_id: &str) -> PathBuf {
        paths.workspace_state(workspace_id).join(LAST_SESSION)
    }

    /// Save execution `id` of `agent` as the workspace's last session.
    pub(crate) fn save(paths: &Paths, workspace_id: &str, agent: &str, id: &str) -> Result<()> {
        Self {
            agent: agent.replace(' ', "-"),
            session: reported_session(paths, workspace_id, id),
            moved: false,
        }
        .write(paths, workspace_id)
    }

    /// Save this as the workspace's last session.
    pub(crate) fn write(&self, paths: &Paths, workspace_id: &str) -> Result<()> {
        std::fs::create_dir_all(paths.workspace_state(workspace_id))?;
        crate::fsutil::replace_atomically(
            &Self::path(paths, workspace_id),
            &serde_json::to_vec(self)?,
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: true,
            },
        )?;
        Ok(())
    }

    pub(crate) fn load(paths: &Paths, workspace_id: &str) -> Result<Option<Self>> {
        let path = Self::path(paths, workspace_id);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .with_context(|| format!("read {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn forget(paths: &Paths, workspace_id: &str) -> Result<()> {
        consume(&Self::path(paths, workspace_id))
    }
}

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

/// Wait for the daemon's verdict. While the daemon restarts, the link
/// reattaches and the next daemon continues the wait.
pub(super) async fn wait(link: &mut super::link::Link) -> Result<Waited> {
    use super::link::{Received, Refused};
    use crate::protocol::Control;
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut quit = signal(SignalKind::quit())?;
    let signalled = || Ok(Waited::Cancelled(Some("interrupted".into())));
    loop {
        tokio::select! {
            received = link.recv() => match received {
                Ok(Received::Detached | Received::Reattached) => {}
                Ok(Received::Control(Control::Resume { ports })) => {
                    // The restored child reattaches once it is registered.
                    link.disarm();
                    return Ok(Waited::Resume(ports));
                }
                Ok(Received::Control(Control::Stop { reason })) => {
                    return Ok(Waited::Cancelled(reason));
                }
                Ok(Received::Control(_)) => anyhow::bail!("unexpected recovery control"),
                Err(error) if error.is::<Refused>() => {
                    return Ok(Waited::Cancelled(Some(error.to_string())));
                }
                Err(error) => return Err(error),
            },
            _ = interrupt.recv() => return signalled(),
            _ = terminate.recv() => return signalled(),
            _ = quit.recv() => return signalled(),
        }
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

    /// The executions of the saved agents and commands.
    pub fn execution_ids(&self) -> Vec<String> {
        self.agents
            .iter()
            .map(|(id, _)| id.clone())
            .chain(self.commands.iter().map(|command| command.id.clone()))
            .collect()
    }

    /// Drop agents whose wrapper is still waiting to restore them, since
    /// `shoal resume` has nothing to do for them.
    pub fn retain_unattended(&mut self) {
        self.agents.retain(|(_, path)| {
            !Record::load(path).is_ok_and(|record| {
                record
                    .and_then(|record| record.waiting)
                    .is_some_and(|wrapper| identity::alive(&wrapper).unwrap_or(false))
            })
        });
    }
}

/// Whether any workspace has stopped work that waits for `shoal resume`;
/// readable while the daemon is down.
pub fn any_pending(paths: &Paths) -> bool {
    let Ok(entries) = std::fs::read_dir(paths.state.join("workspaces")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_name().to_str().is_some_and(|id| {
            Saved::load(paths, id).is_ok_and(|mut saved| {
                saved.retain_unattended();
                !saved.is_empty()
            })
        })
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

/// The executions of stopped agents that wait for `shoal resume` in the workspace.
pub fn agent_records(paths: &Paths, workspace_id: &str) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(paths.workspace_state(workspace_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        if let Some(id) = name.to_string_lossy().strip_suffix(AGENT_SUFFIX) {
            records.push(id.to_owned());
        }
    }
    Ok(records)
}

pub(crate) fn consume(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests;
