//! Restore commands omit the original task and remain available after wrapper exit.
use std::{
    collections::BTreeMap,
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
    sessions::{HostName, Origin},
    state::{AgentState, states},
};

#[derive(Debug)]
pub(crate) struct Recovery {
    pub agent: String,
    pub command: Vec<OsString>,
    pub automatic: bool,
    /// Whether `command` carries the handoff message as the agent's prompt.
    pub handoff_delivered: bool,
    /// The proxy `command` was resolved to go through, from the same
    /// settings.
    pub proxy: Option<crate::proxy::Proxy>,
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
    /// Where the agent ran, as its terminal host records it, so `shoal
    /// resume --all` can restore it there.
    #[serde(
        default,
        alias = "place",
        deserialize_with = "earlier_place",
        skip_serializing_if = "Option::is_none"
    )]
    pub origin: Option<Origin>,
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
    /// What the agent was doing when it stopped, as its hooks last reported
    /// it. Unknown for agents without hooks and for records saved before
    /// turns were kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<AgentState>,
}

/// Why and when Shoal stopped an agent, which its restore tells it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// When its recovery record was saved, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
}

impl Stop {
    /// The stop of the agent `record`, saved at `path`, keeps.
    pub(crate) fn of(record: &Record, path: &Path) -> Self {
        Self {
            reason: record.stop_reason.clone(),
            at: saved_at(path),
        }
    }

    /// The sentences telling the restored agent about the stop.
    fn describe(&self) -> String {
        let at = self
            .at
            .map(|at| format!(" at {}", crate::time::local_minutes(at as i64)))
            .unwrap_or_default();
        let reason = self
            .reason
            .as_deref()
            .map(|reason| format!(": {reason}"))
            .unwrap_or_default();
        format!(
            "The session stopped{at}{reason}. The stop ended the work running in its process, such as subagents and background commands; check what they left before starting them again."
        )
    }
}

/// When the file at `path` was last saved, in Unix seconds, which is when a
/// saved agent stopped.
pub(crate) fn saved_at(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).and_then(|metadata| metadata.modified());
    Some(
        modified
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs(),
    )
}

/// What a restored session is told: why and when it stopped when known, to
/// continue, then `handoff`.
fn resumed_prompt(stop: Option<&Stop>, handoff: Option<&str>) -> String {
    let mut prompt = String::from("Shoal resumed this session. ");
    if let Some(stop) = stop {
        prompt.push_str(&stop.describe());
        prompt.push(' ');
    }
    prompt.push_str("Continue working on the unfinished assignment from where you left off.");
    if let Some(handoff) = handoff {
        prompt.push_str("\n\n");
        prompt.push_str(handoff);
    }
    prompt
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
            Self::resolve(paths, &workspace, agent, None, None, None).await
        }
        .await;
        resolved.unwrap_or_else(|error| {
            eprintln!("warning: automatic agent recovery unavailable: {error:#}");
            Self {
                agent: agent.replace(' ', "-"),
                command: vec![],
                automatic: false,
                handoff_delivered: false,
                proxy: None,
            }
        })
    }

    /// Ask the restored session to continue, telling it about its `stop` and
    /// including `handoff` where the command accepts a prompt: agents with
    /// resume arguments and `{prompt}` in its `resume_command`. Resume
    /// arguments restore `session` when known, otherwise the workspace's
    /// latest session.
    pub(crate) async fn resolve(
        paths: &Paths,
        workspace: &Workspace,
        agent: &str,
        handoff: Option<&str>,
        session: Option<&str>,
        stop: Option<&Stop>,
    ) -> Result<Self> {
        let settings =
            client::settings(paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
        let name = agent.replace(' ', "-");
        let configured = settings.resume_commands.get(&name);
        let resume_args = settings.launch(&name).arguments.resume_args(session);
        let automatic = configured.is_some() || resume_args.is_some();
        let message = resumed_prompt(stop, handoff);
        let prompt = std::ffi::OsStr::new(&message);
        let (command, handoff_delivered) = if let Some(argv) = configured {
            let mut command = configured::expand_with_fields(
                paths,
                &settings.resume_commands,
                &name,
                workspace,
                vec![],
                &[("{prompt}", prompt)],
            )
            .await?;
            // Without Shoal's hooks it still goes through the agent's proxy.
            crate::cli::agents::with_proxy_args(&settings, &name, &mut command);
            (command, argv.iter().any(|arg| arg.contains("{prompt}")))
        } else if let Some(mut args) = resume_args {
            args.extend(
                crate::cli::agents::workspace_args(paths, &settings, &name, workspace).await?,
            );
            let takes_field = settings
                .launchers
                .get(&name)
                .is_some_and(|argv| argv.iter().any(|arg| arg.contains("{prompt}")));
            if !takes_field {
                args.push(prompt.to_owned());
            }
            let command = configured::expand_with_fields(
                paths,
                &settings.launchers,
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
        let proxy = crate::cli::agents::launch_proxy(&settings, &name)?.cloned();
        Ok(Self {
            agent: name,
            command,
            automatic,
            handoff_delivered: handoff_delivered && handoff.is_some(),
            proxy,
        })
    }

    /// The command restoring the agent stopped with `record`, with the
    /// proxy it was resolved to go through: the session it saved, told why
    /// and when it stopped, otherwise the restore command resolved at launch.
    pub(super) async fn restore_command(
        &self,
        paths: &Paths,
        workspace: &Workspace,
        record: Option<&Path>,
    ) -> (Vec<OsString>, Option<crate::proxy::Proxy>) {
        let launched = || (self.command.clone(), self.proxy.clone());
        let Some((path, record)) =
            record.and_then(|path| Some((path, Record::load(path).ok().flatten()?)))
        else {
            return launched();
        };
        let stop = Stop::of(&record, path);
        let session = record.session.as_deref();
        match Self::resolve(paths, workspace, &self.agent, None, session, Some(&stop)).await {
            Ok(recovery) if !recovery.command.is_empty() => (recovery.command, recovery.proxy),
            Ok(_) => launched(),
            Err(error) => {
                eprintln!("warning: cannot resolve the restore command: {error:#}");
                launched()
            }
        }
    }

    /// Save the stopped agent; `restorer` brings it back besides `shoal
    /// resume`.
    pub(super) fn save(
        &self,
        paths: &Paths,
        workspace_id: &str,
        id: &str,
        reason: Option<&str>,
        place: Option<Origin>,
        restorer: Restorer,
    ) -> Result<PathBuf> {
        let record = Record::new(&self.agent, reason, Stage::Stopped, place);
        save_restorable(paths, workspace_id, id, record, restorer)
    }

    /// Save the agent as `save` does before a stop reaches it, so the record
    /// survives this wrapper; saving it once stopped completes the record.
    pub(super) fn save_stopping(
        &self,
        paths: &Paths,
        workspace_id: &str,
        id: &str,
        reason: Option<&str>,
        place: Option<Origin>,
        restorer: Restorer,
    ) -> Result<PathBuf> {
        let record = Record::new(&self.agent, reason, Stage::Stopping, place);
        save_restorable(paths, workspace_id, id, record, restorer)
    }
}

/// Save `record` with what `restorer` needs to bring its agent back.
fn save_restorable(
    paths: &Paths,
    workspace_id: &str,
    id: &str,
    mut record: Record,
    restorer: Restorer,
) -> Result<PathBuf> {
    if let Restorer::Wrapper(wrapper) = &restorer {
        record.waiting = Some(wrapper.clone());
    }
    let path = write_record(paths, workspace_id, id, record)?;
    if let Restorer::Queue(env) = &restorer {
        request_restore(paths, workspace_id, id, env)?;
    }
    Ok(path)
}

/// What brings a stopped agent back besides `shoal resume`.
#[derive(Debug)]
pub(crate) enum Restorer {
    /// Nothing: it stays stopped until the user resumes it.
    User,
    /// This waiting wrapper, once protection lets it.
    Wrapper(Identity),
    /// The agent queue, from which the daemon starts it with these variables
    /// of its terminal host.
    Queue(BTreeMap<String, String>),
}

/// A record's place: an origin, or the bare pane ID records kept before
/// origins, which only Herdr recorded.
fn earlier_place<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Origin>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Place {
        Origin(Origin),
        Pane(String),
    }
    Ok(Some(match Place::deserialize(deserializer)? {
        Place::Origin(origin) => origin,
        Place::Pane(pane) => Origin {
            host: HostName::Herdr,
            group: None,
            place: Some(pane),
            data: BTreeMap::new(),
            owned: false,
            instance: None,
        },
    }))
}

impl Record {
    fn new(agent: &str, reason: Option<&str>, stage: Stage, origin: Option<Origin>) -> Self {
        Self {
            agent: agent.replace(' ', "-"),
            stop_reason: reason.map(str::to_owned),
            stage: Some(stage),
            origin,
            waiting: None,
            session: None,
            moved: false,
            turn: None,
        }
    }

    /// This record, saved at `path`, as the last session of execution `id`,
    /// which keeps its agent, session, place and stop.
    pub(crate) fn into_last_session(self, id: String, path: &Path) -> LastSession {
        let stop = Stop::of(&self, path);
        LastSession {
            execution: id,
            agent: self.agent,
            session: self.session,
            moved: self.moved,
            origin: self.origin,
            stopped_by: None,
            stop: Some(stop),
        }
    }

    /// Whether the agent stopped between turns, with no work in flight. One
    /// whose turn is unknown counts as interrupted mid-turn.
    pub(crate) fn between_turns(&self) -> bool {
        self.turn == Some(AgentState::Idle)
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
            origin: None,
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
    turn: Option<AgentState>,
) -> Result<PathBuf> {
    let record = Record {
        turn,
        ..Record::new(agent, reason, Stage::Stopping, None)
    };
    write_record(paths, workspace_id, id, record)
}

/// Keep `turn`, the state execution `id` last reported, in the recovery
/// record it saved, unless the record already has one. An execution that
/// saved no record stopped nothing.
pub(crate) fn save_turn(
    paths: &Paths,
    workspace_id: &str,
    id: &str,
    turn: AgentState,
) -> Result<()> {
    let Some(record) = Record::load(&record_path(paths, workspace_id, id))? else {
        return Ok(());
    };
    if record.turn.is_some() {
        return Ok(());
    }
    let record = Record {
        turn: Some(turn),
        ..record
    };
    write_record(paths, workspace_id, id, record).map(drop)
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
        record.origin = record.origin.or(saved.origin);
        record.waiting = record.waiting.or(saved.waiting);
        record.session = record.session.or(saved.session);
        record.turn = record.turn.or(saved.turn);
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

/// Ask the daemon to queue the restore of the agent stopped as `id`, which it
/// starts with `env`, the variables of the agent's terminal host, once the
/// execution has ended. The request is its own file beside the record, so
/// forgetting it never replaces a record a resume may hold.
fn request_restore(
    paths: &Paths,
    workspace_id: &str,
    id: &str,
    env: &BTreeMap<String, String>,
) -> Result<()> {
    crate::fsutil::replace_atomically(
        &request_path(paths, workspace_id, id),
        &serde_json::to_vec(env)?,
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: true,
        },
    )?;
    Ok(())
}

/// The workspace's restore requests: each stopped agent's execution ID with
/// the variables its restore starts with. An unreadable request asks for
/// nothing.
pub(crate) fn restore_requests(
    paths: &Paths,
    workspace_id: &str,
) -> Result<Vec<(String, BTreeMap<String, String>)>> {
    let entries = match std::fs::read_dir(paths.workspace_state(workspace_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut requests = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(REQUEST_SUFFIX))
        else {
            continue;
        };
        let env = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        if let Some(env) = env {
            requests.push((id.to_owned(), env));
        }
    }
    requests.sort();
    Ok(requests)
}

/// Forget the restore request of the agent stopped as `id`.
pub(crate) fn forget_restore_request(paths: &Paths, workspace_id: &str, id: &str) -> Result<()> {
    consume(&request_path(paths, workspace_id, id))
}

fn request_path(paths: &Paths, workspace_id: &str, id: &str) -> PathBuf {
    paths
        .workspace_state(workspace_id)
        .join(format!("{id}{REQUEST_SUFFIX}"))
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
/// exited, so none waits to restore them, and the restored workspace waits
/// for `shoal resume` to restore them.
pub(crate) fn stopped_agents(paths: &Paths, workspace_id: &str) -> Result<Vec<StoppedAgent>> {
    let mut agents = Vec::new();
    for (execution, path) in Saved::load(paths, workspace_id)?.agents {
        if let Some(mut record) = Record::load(&path)? {
            record.waiting = None;
            let stopped_at = saved_at(&path).context("read when the agent stopped")?;
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

/// Keep the agent stopped as `id`, whose recovery record is at `path`, as a
/// last session instead, which the process of workspace `stopped_by` stopped
/// when given.
pub(crate) fn keep_as_last_session(
    paths: &Paths,
    workspace_id: &str,
    id: &str,
    path: &Path,
    stopped_by: Option<&str>,
) -> Result<()> {
    let _claim = claim(path)?;
    let Some(record) = Record::load(path)? else {
        return Ok(());
    };
    LastSession {
        stopped_by: stopped_by.map(str::to_owned),
        ..record.into_last_session(id.to_owned(), path)
    }
    .write(paths, workspace_id)?;
    consume(path)
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

/// The workspace whose process stopped the agent that execution `id` of
/// `workspace_id` keeps as a last session, when one did.
pub(crate) fn stopped_by(paths: &Paths, workspace_id: &str, id: &str) -> Result<Option<String>> {
    let path = LastSession::path(paths, workspace_id, id);
    Ok(LastSession::load(&path)?.and_then(|last| last.stopped_by))
}

/// The file keeping the agent stopped as `id`: its recovery record, or else
/// its last session.
pub(crate) fn saved_path(paths: &Paths, workspace_id: &str, id: &str) -> PathBuf {
    let record = record_path(paths, workspace_id, id);
    if record.exists() {
        return record;
    }
    LastSession::path(paths, workspace_id, id)
}

/// Where the agent a recovery record or last session at `path` keeps ran,
/// when known.
pub(crate) fn saved_place(path: &Path) -> Option<Origin> {
    #[derive(Deserialize)]
    struct Placed {
        #[serde(default, alias = "place", deserialize_with = "earlier_place")]
        origin: Option<Origin>,
    }
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<Placed>(&bytes).ok()?.origin
}

pub(crate) fn record_path(paths: &Paths, workspace_id: &str, id: &str) -> PathBuf {
    paths
        .workspace_state(workspace_id)
        .join(format!("{id}{AGENT_SUFFIX}"))
}

const AGENT_SUFFIX: &str = ".recovery.json";
const REQUEST_SUFFIX: &str = ".restore-request.json";
const COMMAND_SUFFIX: &str = ".command.json";
const SESSION_SUFFIX: &str = ".session";
const LAST_SESSION_SUFFIX: &str = ".last-session.json";
const UNNAMED_LAST_SESSION: &str = "last-session.json";

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
pub(crate) fn reported_session(paths: &Paths, workspace_id: &str, id: &str) -> Option<String> {
    std::fs::read_to_string(session_path(paths, workspace_id, id))
        .ok()
        .filter(|session| valid_session(session))
}

/// Forget the session of an execution that has ended.
pub(crate) fn forget_session(paths: &Paths, workspace_id: &str, id: &str) -> Result<()> {
    consume(&session_path(paths, workspace_id, id))
}

/// An agent that exited on its own before its assignment was done, which
/// `shoal resume` continues. Unlike stopped work, it uses no agent slot, never
/// delays cleanup and nothing restores it on its own. A workspace keeps one
/// per agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSession {
    /// The execution the agent ran as, which `--execution` selects. Its file
    /// is named after it; one saved without it gets a new ID.
    #[serde(default = "unnamed_execution")]
    pub execution: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The workspace moved where the session is not available.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moved: bool,
    /// Where the agent ran, as its terminal host records it, so `shoal
    /// resume` can restore it there; unknown for one that exited on its own.
    #[serde(
        default,
        alias = "place",
        deserialize_with = "earlier_place",
        skip_serializing_if = "Option::is_none"
    )]
    pub origin: Option<Origin>,
    /// The workspace whose process stopped the agent with `shoal stop`,
    /// which with the workspaces above it may resume it; unknown for one the
    /// user or Shoal stopped, or that exited on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<String>,
    /// Why and when Shoal stopped the agent; none for one that exited on its
    /// own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Stop>,
}

/// The key of a last session saved without its execution.
fn unnamed_execution() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl LastSession {
    /// This session where it is not available, which starts a new one.
    pub(crate) fn moved(self) -> Self {
        Self {
            session: None,
            moved: true,
            origin: None,
            ..self
        }
    }

    pub(crate) fn path(paths: &Paths, workspace_id: &str, execution: &str) -> PathBuf {
        paths
            .workspace_state(workspace_id)
            .join(format!("{execution}{LAST_SESSION_SUFFIX}"))
    }

    /// Save execution `id` of `agent` as one of the workspace's last sessions.
    pub(crate) fn save(paths: &Paths, workspace_id: &str, agent: &str, id: &str) -> Result<()> {
        Self {
            execution: id.to_owned(),
            agent: agent.replace(' ', "-"),
            session: reported_session(paths, workspace_id, id),
            moved: false,
            origin: None,
            stopped_by: None,
            stop: None,
        }
        .write(paths, workspace_id)
    }

    /// Save this as one of the workspace's last sessions.
    pub(crate) fn write(&self, paths: &Paths, workspace_id: &str) -> Result<()> {
        std::fs::create_dir_all(paths.workspace_state(workspace_id))?;
        crate::fsutil::replace_atomically(
            &Self::path(paths, workspace_id, &self.execution),
            &serde_json::to_vec(self)?,
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: true,
            },
        )?;
        Ok(())
    }

    /// Read the last session saved at `path`, `None` once it is consumed.
    pub(crate) fn load(path: &Path) -> Result<Option<Self>> {
        let execution = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(LAST_SESSION_SUFFIX))
            .with_context(|| format!("{} is not a last session", path.display()))?;
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(|last| {
                    Some(Self {
                        execution: execution.to_owned(),
                        ..last
                    })
                })
                .with_context(|| format!("read {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// The workspace's last sessions; an unreadable one is left out.
    pub(crate) fn all(paths: &Paths, workspace_id: &str) -> Result<Vec<Self>> {
        Ok(Saved::load(paths, workspace_id)?
            .sessions
            .iter()
            .filter_map(|(_, path)| Self::load(path).ok().flatten())
            .collect())
    }

    /// Forget every last session of the workspace.
    pub(crate) fn forget_all(paths: &Paths, workspace_id: &str) -> Result<()> {
        for (_, path) in Saved::load(paths, workspace_id)?.sessions {
            consume(&path)?;
        }
        Ok(())
    }
}

/// Give a workspace's single last session from before workspaces kept one
/// per agent a file of its own. A concurrent reader may already have.
fn name_unnamed_session(paths: &Paths, workspace_id: &str) -> Result<()> {
    let unnamed = paths
        .workspace_state(workspace_id)
        .join(UNNAMED_LAST_SESSION);
    let named = LastSession::path(paths, workspace_id, &unnamed_execution());
    match std::fs::rename(unnamed, named) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Read the last sessions of a removal record, which earlier records kept as
/// a single one.
pub(crate) fn one_or_many_sessions<'de, D>(deserializer: D) -> Result<Vec<LastSession>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Sessions {
        Many(Vec<LastSession>),
        One(Box<LastSession>),
    }
    Ok(match Sessions::deserialize(deserializer)? {
        Sessions::Many(sessions) => sessions,
        Sessions::One(session) => vec![*session],
    })
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
    Resume {
        ports: Vec<crate::model::PortReservation>,
        /// The binary the daemon runs, when it says.
        executable: Option<crate::fsutil::FileIdentity>,
    },
    /// The restore was cancelled, with the daemon's reason if it gave one.
    Cancelled {
        reason: Option<String>,
        /// Removal cancelled it, and the removal record keeps the agent.
        removed: bool,
    },
}

impl Waited {
    fn cancelled(reason: impl Into<String>) -> Self {
        Self::Cancelled {
            reason: Some(reason.into()),
            removed: false,
        }
    }
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
    let signalled = || Ok(Waited::cancelled("interrupted"));
    loop {
        tokio::select! {
            received = link.recv() => match received {
                Ok(Received::Detached | Received::Reattached) => {}
                Ok(Received::Control(Control::Resume { ports, executable })) => {
                    // The restored child reattaches once it is registered.
                    link.disarm();
                    return Ok(Waited::Resume { ports, executable });
                }
                Ok(Received::Control(Control::Stop { reason, removed })) => {
                    return Ok(Waited::Cancelled { reason, removed });
                }
                Ok(Received::Control(_)) => anyhow::bail!("unexpected recovery control"),
                Err(error) if error.is::<Refused>() => {
                    return Ok(Waited::cancelled(error.to_string()));
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
    /// Last sessions by execution ID.
    pub sessions: Vec<(String, PathBuf)>,
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
        name_unnamed_session(paths, workspace_id)?;
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
            } else if let Some(id) = name.strip_suffix(LAST_SESSION_SUFFIX) {
                saved.sessions.push((id.to_owned(), path));
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
        saved.sessions.sort();
        saved.commands.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(saved)
    }

    /// Keep only the record or last session of execution `id`; a selected
    /// agent keeps the workspace's commands too when `with_commands`, so it
    /// receives them.
    pub fn retain_execution(&mut self, id: &str, with_commands: bool) {
        let selected = |(agent, _): &(String, PathBuf)| agent == id;
        if self.agents.iter().chain(&self.sessions).any(selected) {
            self.agents.retain(selected);
            self.sessions.retain(selected);
            if !with_commands {
                self.commands.clear();
            }
        } else {
            self.agents.clear();
            self.sessions.clear();
            self.commands.retain(|command| command.id == id);
        }
    }

    /// Keep only the records of the executions `keep` selects.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.agents.retain(|(id, _)| keep(id));
        self.sessions.retain(|(id, _)| keep(id));
        self.commands.retain(|command| keep(&command.id));
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty() && !self.is_unfinished()
    }

    /// Whether stopped agents or commands are unfinished work, which last
    /// sessions are not.
    pub fn is_unfinished(&self) -> bool {
        !(self.agents.is_empty() && self.commands.is_empty())
    }

    /// The executions of the saved agents, last sessions and commands.
    pub fn execution_ids(&self) -> Vec<String> {
        self.agents
            .iter()
            .chain(&self.sessions)
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
                saved.is_unfinished()
            })
        })
    })
}

/// Pending recovery is unfinished work even when its Git tree is clean.
pub fn pending(paths: &Paths, workspace_id: &str) -> Result<bool> {
    pending_except(paths, workspace_id, |_| false)
}

/// Whether recovery is pending for executions of the workspace other than
/// those `excepted` selects.
pub(crate) fn pending_except(
    paths: &Paths,
    workspace_id: &str,
    excepted: impl Fn(&str) -> bool,
) -> Result<bool> {
    let entries = match std::fs::read_dir(paths.workspace_state(workspace_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let execution = name
            .strip_suffix(AGENT_SUFFIX)
            .or_else(|| name.strip_suffix(COMMAND_SUFFIX));
        if execution.is_some_and(|execution| !excepted(execution)) {
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
