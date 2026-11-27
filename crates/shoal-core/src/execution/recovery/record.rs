//! The recovery record of a stopped agent: saved as a stop reaches the agent,
//! then read, kept as a last session or discarded.
use crate::paths::StateFiles;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::executions::TrackedAgent;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

use super::{
    AGENT_SUFFIX, LastSession, LastSessionExt as _, Record, Stage, Stop, StopExt as _, carried,
    claim, consume, idle, request_restore, rewrite,
};
use crate::{
    agent::{Agent, ModelChoice},
    paths::Paths,
    process::identity::Identity,
    state::AgentState,
};
use shoal_protocol::sessions::Origin;

#[derive(Debug)]
pub struct Recovery {
    pub agent: Agent,
    /// The model and effort the agent runs with, which its restore keeps.
    pub model: ModelChoice,
    /// The mode the agent started in, which its restore keeps.
    pub mode: crate::config::modes::ModeName,
    /// The `--permission` narrowing it started with, which its restore keeps.
    pub narrowing: crate::config::permissions::PermissionsConfig,
    pub command: Vec<OsString>,
    pub automatic: bool,
    /// Whether `command` carries the handoff message as the agent's prompt.
    pub handoff_delivered: bool,
    /// The proxy `command` was resolved to go through, from the same
    /// settings.
    pub proxy: Option<crate::proxy::Proxy>,
}

impl Recovery {
    /// The agent it restores, with the model and effort it runs with.
    pub fn tracked(&self) -> TrackedAgent {
        TrackedAgent {
            name: self.agent.clone(),
            model: self.model.clone(),
            mode: self.mode.clone(),
            narrowing: self.narrowing,
            new_session: false,
        }
    }

    /// Save the agent stopped as `ended` says; `restorer` brings it back
    /// besides `shoal resume`.
    pub fn save(
        &self,
        paths: &Paths,
        space_id: &SpaceId,
        id: &ExecutionId,
        ended: Ended<'_>,
        place: Option<Origin>,
        restorer: Restorer,
    ) -> Result<PathBuf> {
        let record = ended.of(Record::new(
            &self.tracked(),
            ended.reason,
            Stage::Stopped,
            place,
        ));
        save_restorable(paths, space_id, id, record, restorer)
    }

    /// Save the agent as `save` does before a stop reaches it, so the record
    /// survives this wrapper; saving it once stopped completes the record.
    pub fn save_stopping(
        &self,
        paths: &Paths,
        space_id: &SpaceId,
        id: &ExecutionId,
        ended: Ended<'_>,
        place: Option<Origin>,
        restorer: Restorer,
    ) -> Result<PathBuf> {
        let record = Record::new(&self.tracked(), ended.reason, Stage::Stopping, place);
        save_restorable(paths, space_id, id, ended.of(record), restorer)
    }
}

/// Why a wrapper's agent stopped, as its recovery record keeps it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ended<'a> {
    /// The reason `shoal resume` and the restored agent are told.
    pub reason: Option<&'a str>,
    /// What ended it, when the wrapper ended it unasked.
    pub cause: Option<shoal_protocol::stop_causes::StopCause>,
}

impl Ended<'_> {
    /// `record`, keeping what ended its agent.
    fn of(self, record: Record) -> Record {
        Record {
            cause: self.cause,
            ..record
        }
    }
}

/// Save `record` with what `restorer` needs to bring its agent back. The
/// restore request goes first: a request whose record is missing is only
/// forgotten, while a record saved without its request would leave the
/// agent for `shoal resume`, and a save that failed between the two is
/// made again once the record is missing.
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
    if let Restorer::Queue(env) = &restorer {
        request_restore(paths, space_id, id, env)?;
    }
    write_record(paths, space_id, id, record)
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

/// What brings a stopped agent back besides `shoal resume`.
#[derive(Debug)]
pub enum Restorer {
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
        agent: &TrackedAgent,
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
        agent: &TrackedAgent,
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
            handoff: None,
            turn: None,
            model: agent.model.clone(),
            mode: agent.mode.clone(),
            narrowing: agent.narrowing,
            for_load: false,
            oom_kills: 0,
            restores_after: None,
            cause: None,
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
            handoff: self.handoff,
            origin: self.origin,
            stop: Some(stop),
            turn: self.turn,
            model: self.model,
            mode: self.mode,
            narrowing: self.narrowing,
        }
    }

    fn between_turns(&self) -> bool {
        idle(self.turn)
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
    agent: &TrackedAgent,
    reason: Option<&str>,
    turn: Option<AgentState>,
) -> Result<PathBuf> {
    let record = stopping_record(agent, reason, turn);
    write_record(paths, space_id, id, record)
}

/// Save the agent identity as [`save_record`] does before a stop for load or
/// free disk space is delivered, marking the record [`Record::for_load`].
pub fn save_load_record(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    agent: &TrackedAgent,
    reason: Option<&str>,
    turn: Option<AgentState>,
) -> Result<PathBuf> {
    let record = Record {
        for_load: true,
        ..stopping_record(agent, reason, turn)
    };
    write_record(paths, space_id, id, record)
}

/// The record of `agent`, which a stop for `reason` reaches in `turn`.
fn stopping_record(agent: &TrackedAgent, reason: Option<&str>, turn: Option<AgentState>) -> Record {
    Record {
        turn,
        ..Record::new(agent, reason, Stage::Stopping, None)
    }
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

/// Keep in the recovery record of `id` that `kills` OOM kills in a row
/// ended its agent, and the earliest time its restore starts, `after`. An
/// execution that saved no record has nothing to restore.
pub fn save_oom_kills(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    kills: u32,
    after: Option<crate::time::UnixTime>,
) -> Result<()> {
    let Some(record) = Record::load(&record_path(paths, space_id, id))? else {
        return Ok(());
    };
    let record = Record {
        oom_kills: kills,
        restores_after: after,
        ..record
    };
    write_record(paths, space_id, id, record).map(drop)
}

pub(super) fn write_record(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    mut record: Record,
) -> Result<PathBuf> {
    let directory = paths.space_state(space_id);
    std::fs::create_dir_all(&directory)?;
    let path = record_path(paths, space_id, id);
    if record.session.is_none() {
        record = carried::continued(paths, space_id, id).kept_in(record);
    }
    // A later save without a field, such as shutdown racing an overload stop
    // or the daemon's handoff after the wrapper's, keeps the value saved first,
    // and never moves the stage back.
    if let Ok(Some(saved)) = Record::load(&path) {
        record.stop_reason = record.stop_reason.or(saved.stop_reason);
        record.origin = record.origin.or(saved.origin);
        record.waiting = record.waiting.or(saved.waiting);
        record.session = record.session.or(saved.session);
        record.turn = record.turn.or(saved.turn);
        record.for_load |= saved.for_load;
        if record.oom_kills == 0 {
            record.oom_kills = saved.oom_kills;
            record.restores_after = saved.restores_after;
        }
        record.cause = record.cause.or(saved.cause);
        if saved.stage == Some(Stage::Stopped) {
            record.stage = saved.stage;
        }
    }
    rewrite(&path, &record)?;
    Ok(path)
}

/// Keep the agent stopped as `id`, whose recovery record is at `path`, as a
/// last session instead.
pub fn keep_as_last_session(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    path: &Path,
) -> Result<()> {
    let _claim = claim(path)?;
    let Some(record) = Record::load(path)? else {
        return Ok(());
    };
    (record.into_last_session(id.to_owned(), path)).write(paths, space_id)?;
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
