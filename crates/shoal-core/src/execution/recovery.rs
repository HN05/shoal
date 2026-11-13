//! Restore commands omit the original task and remain available after wrapper exit.
use crate::client::execution_ids::ExecutionId;
use crate::client::{agent::AgentSessionId, space_ids::SpaceId};
use crate::config::permissions::PermissionChange;
use crate::paths::StateFiles;
use shoal_refs::BranchName;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

pub mod claim;

pub use claim::{RecoveryClaim, claim, claim_last_session, claim_record, forget_saved};

pub(crate) use crate::client::executions::unnamed_execution;
pub use crate::client::executions::{Copied, LastSession, Record, Stage, Stop, StoppedAgent};
pub use crate::client::saved::{Saved, SavedCommand, SavedSession};
use crate::client::space_names::SpaceSelector;
use crate::{
    agent::{Agent, ModelChoice},
    agents::AgentHome,
    fsutil::atomic::{Permissions, ReplaceOptions},
    model::{ExecutionSpace, Workspace},
    paths::{CommandPaths, Paths},
    process::identity::{self, Identity},
    protocol::ConfigTarget,
    sessions::Origin,
    state::AgentState,
};

#[derive(Debug)]
pub struct Recovery {
    pub agent: Agent,
    /// The model and effort the agent runs with, which its restore keeps.
    pub model: ModelChoice,
    /// The mode the agent started in, which its restore keeps.
    pub(crate) mode: crate::config::modes::ModeName,
    /// The `--permission` narrowing it started with, which its restore keeps.
    pub(crate) narrowing: crate::config::permissions::PermissionsConfig,
    pub command: Vec<OsString>,
    pub automatic: bool,
    /// Whether `command` carries the handoff message as the agent's prompt.
    pub handoff_delivered: bool,
    /// The proxy `command` was resolved to go through, from the same
    /// settings.
    pub(crate) proxy: Option<crate::proxy::Proxy>,
}

/// Operations on the client type [`Stop`] that only the `shoal` command uses.
trait StopExt {
    /// The stop of the agent `record`, saved at `path`, keeps.
    fn of(record: &Record, path: &Path) -> Self;

    /// The sentence telling the restored agent about the stop.
    fn describe(&self) -> String;
}

impl StopExt for Stop {
    fn of(record: &Record, path: &Path) -> Self {
        Self {
            reason: record.stop_reason.clone(),
            at: saved_at(path),
        }
    }

    fn describe(&self) -> String {
        let at = self
            .at
            .map(|at| format!(" at {}", at.local_minutes()))
            .unwrap_or_default();
        let reason = self
            .reason
            .as_deref()
            .map(|reason| format!(" ({reason})"))
            .unwrap_or_default();
        format!("This session stopped{at}{reason}; its subagents and background commands ended.")
    }
}

/// When the file at `path` was last saved, which is when a saved agent
/// stopped.
pub fn saved_at(path: &Path) -> Option<crate::time::UnixTime> {
    let modified = std::fs::metadata(path).and_then(|metadata| metadata.modified());
    Some(crate::time::UnixTime::from_system_time(modified.ok()?))
}

/// What a restored session is told: why and when it stopped when known, to
/// continue, then `handoff`.
fn resumed_prompt(stop: Option<&Stop>, handoff: Option<&str>) -> String {
    let mut prompt = stop.map_or_else(|| "Shoal resumed this session.".to_owned(), Stop::describe);
    prompt.push(' ');
    prompt.push_str("Continue working on the unfinished assignment from where you left off.");
    if let Some(handoff) = handoff {
        prompt.push_str("\n\n");
        prompt.push_str(handoff);
    }
    prompt
}

/// What a new agent of a workspace that moved where its earlier session is
/// not available is told, followed by `handoff`.
pub fn moved_prompt(handoff: Option<&str>) -> String {
    let mut prompt = String::from(
        "Shoal moved this workspace, and your earlier session is not available here. Continue working on the unfinished assignment: read its linked issue and PRs, its commits and its uncommitted changes to see where it stands.",
    );
    if let Some(handoff) = handoff {
        prompt.push_str("\n\n");
        prompt.push_str(handoff);
    }
    prompt
}

/// What an agent of a workspace copied from another is told, from a fork
/// of its session there when `forked`, followed by `handoff`; `copied` says
/// whether the files are older than the conversation.
pub fn copied_prompt(copied: Copied, forked: bool, handoff: Option<&str>) -> String {
    let mut prompt = String::from(
        "Shoal copied this workspace from another onto a branch of its own; the original keeps running with its branch and PRs.",
    );
    prompt.push_str(if forked {
        " This conversation is a fork of its agent's, which continues there separately."
    } else {
        " Its agent's session is not available here."
    });
    if copied.earlier_files {
        prompt.push_str(" The files come from an earlier checkpoint than the conversation, which describes work they do not contain: check them before you act on what you remember.");
    }
    prompt.push_str(" Continue working on the unfinished assignment: read its linked issue, its commits and its uncommitted changes to see where it stands.");
    if let Some(handoff) = handoff {
        prompt.push_str("\n\n");
        prompt.push_str(handoff);
    }
    prompt
}

/// What the agent of a chat that became `workspace` is told, carrying on
/// from a fork of the chat's session when `forked`, followed by `handoff`,
/// before its mode's instructions, which take the fields of the issue the
/// workspace is linked to.
pub fn converted_subject(
    workspace: &Workspace,
    forked: bool,
    handoff: Option<&str>,
) -> crate::config::templates::Subject {
    let mut prompt = format!(
        "Shoal converted this chat into a workspace. You now work in its worktree at {} on branch {}.",
        workspace.path.display(),
        workspace
            .branch
            .as_ref()
            .map(BranchName::as_str)
            .unwrap_or("-"),
    );
    prompt.push_str(if forked {
        " This conversation continues the chat's."
    } else {
        " The chat's session is not available here: read the space's linked issue to see where the work stands."
    });
    if let Some(handoff) = handoff {
        prompt.push_str("\n\n");
        prompt.push_str(handoff);
    }
    crate::config::templates::Subject {
        text: prompt,
        issue: crate::config::templates::IssueFields::linked(&workspace.links),
    }
}

/// What a restore command of agent `name` in `home` is built from: the
/// message it continues with and the arguments that keep its model and
/// effort.
struct RestoreCommand<'a> {
    paths: &'a CommandPaths,
    settings: &'a crate::config::Effective,
    name: &'a Agent,
    home: AgentHome<'a>,
    prompt: &'a std::ffi::OsStr,
    model_args: Vec<OsString>,
    /// The arguments that give the agent its mode's rules again, which only
    /// its built-in resume arguments carry.
    instruction_args: Vec<OsString>,
}

impl RestoreCommand<'_> {
    /// The agent's configured `argv` restore command, with its model, effort
    /// and proxy arguments, and whether it carries the prompt. It is the
    /// user's own, so Shoal adds no rules to it.
    async fn configured(self, argv: &[String]) -> Result<(Vec<OsString>, bool)> {
        let mut command = (self.home)
            .expand(
                self.paths,
                &self.settings.resume_commands,
                self.name.name(),
                vec![],
                &[("{prompt}", self.prompt)],
            )
            .await?;
        crate::agents::insert_options(&mut command, self.model_args);
        // Without Shoal's hooks it still goes through the agent's proxy.
        crate::agents::with_proxy_args(self.settings, self.name, &mut command);
        Ok((command, argv.iter().any(|arg| arg.contains("{prompt}"))))
    }

    /// The agent's launcher with `resume_args`, its model, effort, rules and
    /// its space's arguments, and the prompt.
    async fn resumed(self, mut args: Vec<OsString>) -> Result<Vec<OsString>> {
        args.extend(self.model_args);
        args.extend(self.instruction_args);
        args.extend(self.home.args(self.paths, self.settings, self.name).await?);
        let takes_field = self
            .settings
            .launchers
            .get(self.name.name())
            .is_some_and(|argv| argv.iter().any(|arg| arg.contains("{prompt}")));
        if !takes_field {
            args.push(self.prompt.to_owned());
        }
        (self.home)
            .expand(
                self.paths,
                &self.settings.launchers,
                self.name.name(),
                args,
                &[("{prompt}", self.prompt)],
            )
            .await
    }
}

/// The agent a restore continues: its name, the model and effort it ran
/// with, the mode and narrowing it started with, and the session it
/// reported, if any.
pub struct Restored<'a> {
    pub agent: &'a Agent,
    pub model: &'a ModelChoice,
    pub mode: &'a crate::config::modes::ModeName,
    pub narrowing: crate::config::permissions::PermissionsConfig,
    pub session: Option<&'a AgentSessionId>,
}

impl Recovery {
    /// The agent it restores, with the model and effort it runs with.
    pub fn tracked(&self) -> super::TrackedAgent {
        super::TrackedAgent {
            name: self.agent.clone(),
            model: self.model.clone(),
            mode: self.mode.clone(),
            narrowing: self.narrowing,
        }
    }

    pub(super) async fn for_launch(
        paths: &CommandPaths,
        workspace: &SpaceSelector,
        agent: &super::TrackedAgent,
    ) -> Self {
        let restored = Restored {
            agent: &agent.name,
            model: &agent.model,
            mode: &agent.mode,
            narrowing: agent.narrowing,
            session: None,
        };
        let resolved = Self::resolve_in(paths, workspace, restored, None).await;
        resolved.unwrap_or_else(|error| {
            eprintln!("warning: automatic agent recovery unavailable: {error:#}");
            Self {
                agent: agent.name.normalized(),
                model: agent.model.clone(),
                mode: agent.mode.clone(),
                narrowing: agent.narrowing,
                command: vec![],
                automatic: false,
                handoff_delivered: false,
                proxy: None,
            }
        })
    }

    /// [`Self::resolve`] in the space `space` as the daemon records it now.
    async fn resolve_in(
        paths: &CommandPaths,
        space: &SpaceSelector,
        restored: Restored<'_>,
        stop: Option<&Stop>,
    ) -> Result<Self> {
        let records = crate::agents::HomeRecords::read(paths, space).await?;
        Self::resolve(paths, records.home()?, restored, None, stop).await
    }

    /// Ask the `restored` session to continue, telling it about its `stop`
    /// and including `handoff` where the command accepts a prompt: agents
    /// with resume arguments and `{prompt}` in its `resume_command`. Resume
    /// arguments restore its session when known, otherwise the workspace's
    /// latest session, with its model and effort, in `home`, a workspace or
    /// a chat.
    pub async fn resolve<'a>(
        paths: &CommandPaths,
        home: impl Into<AgentHome<'a>>,
        restored: Restored<'_>,
        handoff: Option<&str>,
        stop: Option<&Stop>,
    ) -> Result<Self> {
        let home = home.into();
        let settings =
            crate::config::settings(paths, ConfigTarget::Space(SpaceSelector::id(home.id())))
                .await?;
        let name = restored.agent.normalized();
        // A restore of an agent config turned off waits, as one it cannot
        // run with its model does.
        anyhow::ensure!(
            !settings.turned_off(&name),
            "{name} is turned off: its [agents] entry sets enabled = false"
        );
        let configured = settings.resume_commands.get(name.name());
        let resume_args = settings
            .launch(&name)
            .arguments
            .resume_args(restored.session);
        let automatic = configured.is_some() || resume_args.is_some();
        let message = resumed_prompt(stop, handoff);
        let restore = RestoreCommand {
            paths,
            settings: &settings,
            name: &name,
            home,
            prompt: std::ffi::OsStr::new(&message),
            // A restore runs with what the agent ran with, or not at all, so
            // the record stays for once its templates take the values again.
            model_args: crate::agents::model_args(&settings, &name, restored.model)?,
            instruction_args: crate::agents::restored_instruction_args(
                &settings,
                &name,
                home,
                restored.mode,
            ),
        };
        let (command, handoff_delivered) = match (configured, resume_args) {
            (Some(argv), _) => restore.configured(argv).await?,
            (None, Some(args)) => (restore.resumed(args).await?, true),
            (None, None) => (vec![], false),
        };
        let proxy = crate::agents::launch_proxy(&settings, &name)?.cloned();
        Ok(Self {
            agent: name,
            model: restored.model.clone(),
            mode: restored.mode.to_owned(),
            narrowing: restored.narrowing,
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
        paths: &CommandPaths,
        space: &ExecutionSpace,
        record: Option<&Path>,
    ) -> (Vec<OsString>, Option<crate::proxy::Proxy>) {
        let launched = || (self.command.clone(), self.proxy.clone());
        let Some((path, record)) =
            record.and_then(|path| Some((path, Record::load(path).ok().flatten()?)))
        else {
            return launched();
        };
        let stop = Stop::of(&record, path);
        let restored = Restored {
            narrowing: self.narrowing,
            agent: &self.agent,
            model: &self.model,
            mode: &self.mode,
            session: record.session.as_ref(),
        };
        let space = SpaceSelector::id(&space.id);
        match Self::resolve_in(paths, &space, restored, Some(&stop)).await {
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
        space_id: &SpaceId,
        id: &ExecutionId,
        reason: Option<&str>,
        place: Option<Origin>,
        restorer: Restorer,
    ) -> Result<PathBuf> {
        let record = Record::new(&self.tracked(), reason, Stage::Stopped, place);
        save_restorable(paths, space_id, id, record, restorer)
    }

    /// Save the agent as `save` does before a stop reaches it, so the record
    /// survives this wrapper; saving it once stopped completes the record.
    pub(super) fn save_stopping(
        &self,
        paths: &Paths,
        space_id: &SpaceId,
        id: &ExecutionId,
        reason: Option<&str>,
        place: Option<Origin>,
        restorer: Restorer,
    ) -> Result<PathBuf> {
        let record = Record::new(&self.tracked(), reason, Stage::Stopping, place);
        save_restorable(paths, space_id, id, record, restorer)
    }
}

/// Save `record` with what `restorer` needs to bring its agent back.
fn save_restorable(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    mut record: Record,
    restorer: Restorer,
) -> Result<PathBuf> {
    if let Restorer::Wrapper(wrapper) = &restorer {
        record.waiting = Some(wrapper.clone());
    }
    let path = write_record(paths, space_id, id, record)?;
    if let Restorer::Queue(env) = &restorer {
        request_restore(paths, space_id, id, env)?;
    }
    Ok(path)
}

/// Save `record`, an agent stopped as `id` without a wrapper, such as a
/// chat's an upgrade stopped, with a request to restore it with `env`,
/// the variables of its terminal host, when it comes back on its own.
pub fn save_stopped(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    record: Record,
    restore: Option<BTreeMap<String, String>>,
) -> Result<PathBuf> {
    let restorer = match restore {
        Some(env) => Restorer::Queue(env),
        None => Restorer::User,
    };
    let record = Record {
        stage: Some(Stage::Stopped),
        ..record
    };
    save_restorable(paths, space_id, id, record, restorer)
}

/// What brings a stopped agent back besides `shoal restore`.
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

/// Operations on the client type [`Record`] that only the `shoal` command uses.
pub trait RecordExt: Sized {
    /// The record of `agent`, stopped for `reason`.
    fn new(
        agent: &super::TrackedAgent,
        reason: Option<&str>,
        stage: Stage,
        origin: Option<Origin>,
    ) -> Self;

    /// This record, saved at `path`, as the last session of execution `id`,
    /// which keeps its agent, session, place and stop.
    fn into_last_session(self, id: ExecutionId, path: &Path) -> LastSession;

    /// Whether the agent stopped between turns, with no work in flight. One
    /// whose turn is unknown counts as interrupted mid-turn.
    fn between_turns(&self) -> bool;

    /// This record where neither its session nor its place exists, such as
    /// on another machine, and no wrapper waits to restore it.
    fn moved(self) -> Self;

    /// This record on another machine, where its place does not exist and no
    /// wrapper waits to restore it, but its session may.
    fn elsewhere(self) -> Self;

    /// Read a saved record, `None` once it is consumed.
    fn load(path: &Path) -> Result<Option<Self>>;
}

impl RecordExt for Record {
    fn new(
        agent: &super::TrackedAgent,
        reason: Option<&str>,
        stage: Stage,
        origin: Option<Origin>,
    ) -> Self {
        Self {
            agent: agent.name.normalized(),
            stop_reason: reason.map(str::to_owned),
            stage: Some(stage),
            origin,
            waiting: None,
            session: None,
            moved: false,
            copied: None,
            converted: false,
            stopped_by: None,
            turn: None,
            model: agent.model.clone(),
            mode: agent.mode.clone(),
            narrowing: agent.narrowing,
        }
    }

    fn into_last_session(self, id: ExecutionId, path: &Path) -> LastSession {
        let stop = Stop::of(&self, path);
        LastSession {
            execution: id,
            agent: self.agent,
            session: self.session,
            moved: self.moved,
            copied: self.copied,
            converted: self.converted,
            origin: self.origin,
            stopped_by: self.stopped_by,
            stop: Some(stop),
            model: self.model,
            mode: self.mode,
            narrowing: self.narrowing,
        }
    }

    fn between_turns(&self) -> bool {
        self.turn == Some(AgentState::Idle)
    }

    fn moved(self) -> Self {
        Self {
            session: None,
            moved: true,
            ..self.elsewhere()
        }
    }

    fn elsewhere(self) -> Self {
        Self {
            origin: None,
            waiting: None,
            ..self
        }
    }

    fn load(path: &Path) -> Result<Option<Self>> {
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
pub fn save_record(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    agent: &super::TrackedAgent,
    reason: Option<&str>,
    turn: Option<AgentState>,
) -> Result<PathBuf> {
    save_record_stopped_by(paths, space_id, id, agent, reason, turn, None)
}

/// Save the record as [`save_record`] does, for a stop the process of the
/// space `stopped_by` asked for when given, which may resume the agent.
pub fn save_record_stopped_by(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    agent: &super::TrackedAgent,
    reason: Option<&str>,
    turn: Option<AgentState>,
    stopped_by: Option<&SpaceId>,
) -> Result<PathBuf> {
    let record = Record {
        turn,
        stopped_by: stopped_by.cloned(),
        ..Record::new(agent, reason, Stage::Stopping, None)
    };
    write_record(paths, space_id, id, record)
}

/// Keep `turn`, the state execution `id` last reported, in the recovery
/// record it saved, unless the record already has one. An execution that
/// saved no record stopped nothing.
pub fn save_turn(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    turn: AgentState,
) -> Result<()> {
    let Some(record) = Record::load(&record_path(paths, space_id, id))? else {
        return Ok(());
    };
    if record.turn.is_some() {
        return Ok(());
    }
    let record = Record {
        turn: Some(turn),
        ..record
    };
    write_record(paths, space_id, id, record).map(drop)
}

fn write_record(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    mut record: Record,
) -> Result<PathBuf> {
    let directory = paths.space_state(space_id);
    std::fs::create_dir_all(&directory)?;
    let path = record_path(paths, space_id, id);
    record.session = record
        .session
        .or_else(|| reported_session(paths, space_id, id));
    // A later save without a field, such as shutdown racing an overload stop
    // or the daemon's handoff after the wrapper's, keeps the value saved first,
    // and never moves the stage back.
    if let Ok(Some(saved)) = Record::load(&path) {
        record.stop_reason = record.stop_reason.or(saved.stop_reason);
        record.origin = record.origin.or(saved.origin);
        record.waiting = record.waiting.or(saved.waiting);
        record.session = record.session.or(saved.session);
        record.turn = record.turn.or(saved.turn);
        record.stopped_by = record.stopped_by.or(saved.stopped_by);
        if saved.stage == Some(Stage::Stopped) {
            record.stage = saved.stage;
        }
    }
    crate::fsutil::atomic::replace_atomically(
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
    space_id: &SpaceId,
    id: &ExecutionId,
    env: &BTreeMap<String, String>,
) -> Result<()> {
    crate::fsutil::atomic::replace_atomically(
        &request_path(paths, space_id, id),
        &serde_json::to_vec(env)?,
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: true,
        },
    )?;
    Ok(())
}

/// The space's restore requests: each stopped agent's execution ID with
/// the variables its restore starts with. An unreadable request asks for
/// nothing.
pub fn restore_requests(
    paths: &Paths,
    space_id: &SpaceId,
) -> Result<Vec<(ExecutionId, BTreeMap<String, String>)>> {
    let entries = match std::fs::read_dir(paths.space_state(space_id)) {
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
            requests.push((ExecutionId::from(id.to_owned()), env));
        }
    }
    requests.sort();
    Ok(requests)
}

/// Forget the restore request of the agent stopped as `id`.
pub fn forget_restore_request(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> Result<()> {
    consume(&request_path(paths, space_id, id))
}

fn request_path(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> PathBuf {
    paths
        .space_state(space_id)
        .join(format!("{id}{REQUEST_SUFFIX}"))
}

/// Whether a stop saved an agent of the space, as a recovery record or a
/// last session. It reads only the file names, so a record that does not
/// parse still counts.
pub fn saves_agent(paths: &Paths, space_id: &SpaceId) -> Result<bool> {
    let entries = match std::fs::read_dir(paths.space_state(space_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.ends_with(AGENT_SUFFIX)
            || name.ends_with(LAST_SESSION_SUFFIX)
            || name == UNNAMED_LAST_SESSION
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The workspace's stopped agents, for its removal record. Their wrappers have
/// exited, so none waits to restore them, and the restored workspace waits
/// for `shoal restore` to restore them.
pub fn stopped_agents(paths: &Paths, space_id: &SpaceId) -> Result<Vec<StoppedAgent>> {
    let mut agents = Vec::new();
    for SavedSession {
        id: execution,
        path,
        ..
    } in Saved::load(paths, space_id)?.agents
    {
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
pub fn save_stopped_agents(
    paths: &Paths,
    space_id: &SpaceId,
    agents: &[StoppedAgent],
) -> Result<()> {
    for agent in agents {
        let path = write_record(paths, space_id, &agent.execution, agent.record.clone())?;
        std::fs::File::open(path)?.set_modified(agent.stopped_at.system_time())?;
    }
    Ok(())
}

/// Keep the agent stopped as `id`, whose recovery record is at `path`, as a
/// last session instead, which the process of space `stopped_by` stopped
/// when given, or else the space its record names.
pub fn keep_as_last_session(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    path: &Path,
    stopped_by: Option<&SpaceId>,
) -> Result<()> {
    let _claim = claim(path)?;
    let Some(record) = Record::load(path)? else {
        return Ok(());
    };
    let last = record.into_last_session(id.to_owned(), path);
    LastSession {
        stopped_by: stopped_by.cloned().or(last.stopped_by),
        ..last
    }
    .write(paths, space_id)?;
    consume(path)
}

/// Remove a record whose stop never reached the agent, returning whether it
/// did. A record the wrapper saved after stopping the agent is kept.
pub fn discard_undelivered(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> Result<bool> {
    let path = record_path(paths, space_id, id);
    match Record::load(&path)? {
        Some(record) if record.stage == Some(Stage::Stopping) => {
            consume(&path)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The file keeping the agent stopped as `id`: its recovery record, or else
/// its last session.
pub fn saved_path(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> PathBuf {
    let record = record_path(paths, space_id, id);
    if record.exists() {
        return record;
    }
    LastSession::path(paths, space_id, id)
}

pub fn record_path(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> PathBuf {
    paths
        .space_state(space_id)
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
pub fn valid_session(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && !session.starts_with('-')
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn session_path(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> PathBuf {
    paths
        .space_state(space_id)
        .join(format!("{id}{SESSION_SUFFIX}"))
}

/// Record the session the hooks of execution `id` report, writing only a
/// change; whether it changed.
pub fn save_session(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    session: &AgentSessionId,
) -> Result<bool> {
    if reported_session(paths, space_id, id).as_ref() == Some(session) {
        return Ok(false);
    }
    std::fs::create_dir_all(paths.space_state(space_id))?;
    crate::fsutil::atomic::replace_atomically(
        &session_path(paths, space_id, id),
        session.as_str().as_bytes(),
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: false,
        },
    )?;
    Ok(true)
}

/// The session the hooks of execution `id` last reported.
pub fn reported_session(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
) -> Option<AgentSessionId> {
    std::fs::read_to_string(session_path(paths, space_id, id))
        .ok()
        .filter(|session| valid_session(session))
        .map(AgentSessionId::from)
}

/// Forget the session of an execution that has ended.
pub fn forget_session(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> Result<()> {
    consume(&session_path(paths, space_id, id))
}

/// Operations on the client type [`LastSession`] that only the `shoal` command uses.
pub trait LastSessionExt: Sized {
    /// This session where it is not available, which starts a new one.
    fn moved(self) -> Self;

    fn path(paths: &Paths, space_id: &SpaceId, execution: &ExecutionId) -> PathBuf;

    /// Save execution `id` of `agent` as one of the space's last
    /// sessions.
    fn save(
        paths: &Paths,
        space_id: &SpaceId,
        agent: &super::TrackedAgent,
        id: &ExecutionId,
    ) -> Result<()>;

    /// Save this as one of the space's last sessions.
    fn write(&self, paths: &Paths, space_id: &SpaceId) -> Result<()>;

    /// Read the last session saved at `path`, `None` once it is consumed.
    fn load(path: &Path) -> Result<Option<Self>>;

    /// The space's last sessions; an unreadable one is left out.
    fn all(paths: &Paths, space_id: &SpaceId) -> Result<Vec<Self>>;

    /// Forget every last session of the space.
    fn forget_all(paths: &Paths, space_id: &SpaceId) -> Result<()>;
}

impl LastSessionExt for LastSession {
    fn moved(self) -> Self {
        Self {
            session: None,
            moved: true,
            origin: None,
            ..self
        }
    }

    fn path(paths: &Paths, space_id: &SpaceId, execution: &ExecutionId) -> PathBuf {
        paths
            .space_state(space_id)
            .join(format!("{execution}{LAST_SESSION_SUFFIX}"))
    }

    fn save(
        paths: &Paths,
        space_id: &SpaceId,
        agent: &super::TrackedAgent,
        id: &ExecutionId,
    ) -> Result<()> {
        Self {
            execution: id.to_owned(),
            agent: agent.name.normalized(),
            session: reported_session(paths, space_id, id),
            moved: false,
            copied: None,
            converted: false,
            origin: None,
            stopped_by: None,
            stop: None,
            model: agent.model.clone(),
            mode: agent.mode.clone(),
            narrowing: agent.narrowing,
        }
        .write(paths, space_id)
    }

    fn write(&self, paths: &Paths, space_id: &SpaceId) -> Result<()> {
        std::fs::create_dir_all(paths.space_state(space_id))?;
        crate::fsutil::atomic::replace_atomically(
            &Self::path(paths, space_id, &self.execution),
            &serde_json::to_vec(self)?,
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: true,
            },
        )?;
        Ok(())
    }

    fn load(path: &Path) -> Result<Option<Self>> {
        let execution = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(LAST_SESSION_SUFFIX))
            .with_context(|| format!("{} is not a last session", path.display()))?;
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(|last| {
                    Some(Self {
                        execution: ExecutionId::from(execution.to_owned()),
                        ..last
                    })
                })
                .with_context(|| format!("read {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn all(paths: &Paths, space_id: &SpaceId) -> Result<Vec<Self>> {
        Ok(Saved::load(paths, space_id)?
            .sessions
            .iter()
            .filter_map(|last| Self::load(&last.path).ok().flatten())
            .collect())
    }

    fn forget_all(paths: &Paths, space_id: &SpaceId) -> Result<()> {
        for last in Saved::load(paths, space_id)?.sessions {
            consume(&last.path)?;
        }
        Ok(())
    }
}

/// Give a workspace's single last session from before workspaces kept one
/// per agent a file of its own. A concurrent reader may already have.
fn name_unnamed_session(paths: &Paths, space_id: &SpaceId) -> Result<()> {
    let unnamed = paths.space_state(space_id).join(UNNAMED_LAST_SESSION);
    let named = LastSession::path(paths, space_id, &unnamed_execution());
    match std::fs::rename(unnamed, named) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// A command `shoal stop` interrupted, reported by `shoal restore` instead of
/// being rerun: replaying an arbitrary command is not known to be safe.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CommandRecord {
    pub(crate) argv: Vec<String>,
}

pub(super) fn save_command(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
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
    let directory = paths.space_state(space_id);
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{id}{COMMAND_SUFFIX}"));
    crate::fsutil::atomic::replace_atomically(
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
                Ok(Received::Control(Control::RestoreWaits { issue })) => {
                    eprintln!("shoal: the restore also waits until the agent's remaining processes exit: {issue}");
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

/// Reading what a workspace saved, which the daemon answers requests with.
pub trait SavedExt: Sized {
    fn load(paths: &Paths, space_id: &SpaceId) -> Result<Self>;
}

impl SavedExt for Saved {
    fn load(paths: &Paths, space_id: &SpaceId) -> Result<Self> {
        let mut saved = Self::default();
        name_unnamed_session(paths, space_id)?;
        let entries = match std::fs::read_dir(paths.space_state(space_id)) {
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
                let id = ExecutionId::from(id.to_owned());
                saved.agents.push(stopped_agent(id, path));
            } else if let Some(id) = name.strip_suffix(LAST_SESSION_SUFFIX) {
                let id = ExecutionId::from(id.to_owned());
                saved.sessions.push(last_session(id, path));
            } else if let Some(id) = name.strip_suffix(COMMAND_SUFFIX) {
                let record: CommandRecord = serde_json::from_slice(&std::fs::read(&path)?)
                    .with_context(|| format!("read {}", path.display()))?;
                saved.commands.push(SavedCommand {
                    id: ExecutionId::from(id.to_owned()),
                    path,
                    argv: record.argv,
                });
            }
        }
        saved.agents.sort_by(|a, b| a.id.cmp(&b.id));
        saved.sessions.sort_by(|a, b| a.id.cmp(&b.id));
        saved.commands.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(saved)
    }
}

/// The stopped agent `id` whose record is at `path`.
fn stopped_agent(id: ExecutionId, path: PathBuf) -> SavedSession {
    let record = Record::load(&path).ok().flatten();
    SavedSession {
        stopped_at: saved_at(&path),
        agent: record.as_ref().map(|record| record.agent.clone()),
        stop_reason: record
            .as_ref()
            .and_then(|record| record.stop_reason.clone()),
        idle: record.as_ref().is_some_and(Record::between_turns),
        waits_in_wrapper: record.as_ref().is_some_and(waits),
        origin: record.and_then(|record| record.origin),
        id,
        path,
    }
}

/// The last session `id` saved at `path`, whose agent exited, so it
/// stopped neither idle nor for a reason Shoal knows.
fn last_session(id: ExecutionId, path: PathBuf) -> SavedSession {
    let last = LastSession::load(&path).ok().flatten();
    SavedSession {
        stopped_at: saved_at(&path),
        agent: last.as_ref().map(|last| last.agent.clone()),
        stop_reason: None,
        idle: false,
        waits_in_wrapper: false,
        origin: last.and_then(|last| last.origin),
        id,
        path,
    }
}

/// Make `change` to the permissions given to each agent and last session
/// the `saved` lists keep, which its restore runs under, holding the claim a
/// restore takes on each of them, so one a restore holds is refused before
/// any changes.
pub(crate) fn change_permissions(saved: &[Saved], change: &PermissionChange) -> Result<()> {
    ClaimedChange::claim(saved)?.make(change)
}

/// The agents and last sessions a permission change rewrites, each claimed
/// as a restore claims it until the change is made.
pub struct ClaimedChange<'a> {
    saved: &'a [Saved],
    _claims: Vec<RecoveryClaim>,
}

impl<'a> ClaimedChange<'a> {
    /// Claim each agent and last session `saved` lists, refusing one a
    /// restore holds.
    pub fn claim(saved: &'a [Saved]) -> Result<Self> {
        let paths = saved
            .iter()
            .flat_map(|saved| saved.agents.iter().chain(&saved.sessions));
        let claims = paths
            .map(|saved| claim(&saved.path))
            .collect::<Result<_>>()?;
        Ok(Self {
            saved,
            _claims: claims,
        })
    }

    /// Make `change` to the permissions each was given.
    pub fn make(self, change: &PermissionChange) -> Result<()> {
        let agents = self.saved.iter().flat_map(|saved| &saved.agents);
        let sessions = self.saved.iter().flat_map(|saved| &saved.sessions);
        for SavedSession { path, .. } in agents {
            if let Some(record) = Record::load(path)? {
                let narrowing = change.applied_to(record.narrowing);
                rewrite(
                    path,
                    &Record {
                        narrowing,
                        ..record
                    },
                )?;
            }
        }
        for SavedSession { path, .. } in sessions {
            if let Some(last) = LastSession::load(path)? {
                let narrowing = change.applied_to(last.narrowing);
                rewrite(path, &LastSession { narrowing, ..last })?;
            }
        }
        Ok(())
    }
}

/// Make `change` to the permissions given to the saved agent or last
/// session of execution `id` of the space `space_id`.
pub fn change_permission(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    change: &PermissionChange,
) -> Result<()> {
    let mut saved = Saved::load(paths, space_id)?;
    saved.retain_execution(id, false);
    change_permissions(&[saved], change)
}

/// Save `saved` again at `path`, where a stopped agent's file is.
fn rewrite(path: &Path, saved: &impl Serialize) -> Result<()> {
    crate::fsutil::atomic::replace_atomically(
        path,
        &serde_json::to_vec(saved)?,
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: true,
        },
    )?;
    Ok(())
}

/// Whether a live wrapper waits to restore the agent of `record`.
fn waits(record: &Record) -> bool {
    (record.waiting.as_ref()).is_some_and(|wrapper| identity::alive(wrapper).unwrap_or(false))
}

/// Whether any workspace has stopped work that waits for `shoal restore`;
/// readable while the daemon is down.
pub fn any_pending(paths: &Paths) -> bool {
    let Ok(entries) = std::fs::read_dir(paths.state.join("workspaces")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_name().to_str().is_some_and(|id| {
            let saved = Saved::load(paths, &SpaceId::from(id.to_owned()));
            saved.is_ok_and(|saved| saved.is_restorable())
        })
    })
}

/// Pending recovery is unfinished work even when its Git tree is clean.
pub fn pending(paths: &Paths, space_id: &SpaceId) -> Result<bool> {
    pending_except(paths, space_id, |_| false)
}

/// Whether recovery is pending for executions of the space other than
/// those `excepted` selects.
pub fn pending_except(
    paths: &Paths,
    space_id: &SpaceId,
    excepted: impl Fn(&ExecutionId) -> bool,
) -> Result<bool> {
    let entries = match std::fs::read_dir(paths.space_state(space_id)) {
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
        if execution.is_some_and(|execution| !excepted(&ExecutionId::from(execution.to_owned()))) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The executions of stopped agents that wait for `shoal restore` in the space.
pub fn agent_records(paths: &Paths, space_id: &SpaceId) -> Result<Vec<ExecutionId>> {
    let entries = match std::fs::read_dir(paths.space_state(space_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        if let Some(id) = name.to_string_lossy().strip_suffix(AGENT_SUFFIX) {
            records.push(ExecutionId::from(id.to_owned()));
        }
    }
    Ok(records)
}

pub fn consume(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests;
