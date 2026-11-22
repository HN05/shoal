//! Starting Shoal work beside this terminal: additions, tracked agents and
//! chats, focused or in the background.
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::executions::TrackedAgent;
use shoal_protocol::space_names::SpaceSelector;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

use super::Caller;
use crate::places::{groups, naming::name_plan};
use crate::{
    config::Effective,
    env,
    model::{Part, Space},
    start_plan::{PlacePlan, open_place},
    terminal::{
        Attaches, Configured, Owner, Placement, Places, Start, chat_session_name, current,
        left_open, place_env, report_detach,
    },
};

/// Start `plan` in `space` beside this terminal as its start placed it,
/// focused or in the background. Returns whether it left this process; a
/// focused launch without a new tab stays here, where its agent runs in a
/// session this terminal attaches to.
pub async fn start_place(
    caller: Caller<'_>,
    settings: &Effective,
    placement: Placement,
    plan: &mut PlacePlan,
    space: &Space,
) -> Result<bool> {
    ensure!(
        !crate::process::scoped::inherits_scope(&caller.paths.state),
        "workspace processes open no place beside a terminal"
    );
    let host = caller.launcher().host(settings);
    name_plan(
        caller.paths,
        host.options.name_template.clone(),
        plan,
        space,
    )
    .await?;
    let Some(places) = host.places() else {
        return Ok(false);
    };
    // A focused start stays here where the host keeps agents in sessions
    // this terminal attaches to, so its agent starts in one.
    let focus = placement == Placement::Focus;
    if focus && host.host.attaches().is_some() {
        return Ok(false);
    }
    let cwd = std::env::current_dir()?;
    let repo = groups::Repo::of_space(space);
    let origin = groups::beside(caller.paths, &host, &repo, &cwd).await?;
    let beside = host.beside(origin.as_ref());
    open_place(caller.paths, &host, places, beside, plan, space, focus).await?;
    if !focus && !caller.json {
        let name = space.name.as_str();
        shoal_core::outln!("Started {name} {}", places.background_place(name));
    }
    Ok(true)
}

/// Where a tracked agent launched from this terminal runs apart from this
/// process.
enum AgentRun {
    /// In a new session this terminal attaches to.
    Session(&'static dyn Attaches),
    /// In a new place of a host that does not serve this terminal, shown
    /// here.
    Place(&'static dyn Places),
}

impl AgentRun {
    /// Where `host` runs the agent, or `None` for in place, as in a place
    /// of a host serving this terminal.
    fn of(host: &Configured) -> Option<Self> {
        if let Some(attaches) = host.host.attaches() {
            return Some(Self::Session(attaches));
        }
        host.places().filter(|_| !host.serves()).map(Self::Place)
    }
}

/// The space a tracked agent runs for, as a host places the agent: beside
/// the spaces of its repository, in its directory, under its name.
pub struct AgentSpace<'a> {
    /// The repository whose spaces its place opens beside, or the
    /// `root_dir` of a chat without one.
    repo: groups::Repo<'a>,
    /// Where the agent works.
    directory: &'a Path,
    /// The name its place shows.
    label: &'a str,
    /// The name of a session it runs in, which never changes.
    session: String,
    /// The number `shoal attach` takes it by.
    number: SpaceNumber,
    owner: Owner<'a>,
}

impl<'a> AgentSpace<'a> {
    /// What a host reads of `space`, a workspace or a chat. A workspace's
    /// session is named after its worktree directory.
    pub fn of<P: Part>(space: &'a Space<P>) -> Self {
        let directory = space.workdir.path();
        let repo = match &space.repository_id {
            Some(id) => groups::Repo::Id(id.as_str()),
            None => groups::Repo::Root(directory),
        };
        let (owner, session) = match space.worktree() {
            Some(worktree) => (
                Owner::Workspace(&space.id),
                worktree.directory_name().to_owned(),
            ),
            None => (Owner::Chat(&space.id), chat_session_name(&space.id)),
        };
        Self {
            repo,
            directory,
            label: &space.name,
            session,
            number: space.number,
            owner,
        }
    }

    /// What a host reads of the space an agent runs in at `home`.
    pub fn of_home(home: crate::agents::AgentHome<'a>) -> Self {
        match home {
            crate::agents::AgentHome::Workspace(workspace) => Self::of(workspace),
            crate::agents::AgentHome::Chat { chat, .. } => Self::of(chat),
        }
    }

    /// Whom the agent's session or place is for.
    pub fn owner(&self) -> Owner<'a> {
        self.owner
    }
}

/// Run a tracked agent's `command` for `space` apart from this process,
/// where the host for this terminal runs agents so, and show it here.
/// `None` means the agent runs in place in this process. `records` are
/// recovery records the launch consumes once it starts.
pub async fn run_agent(
    caller: Caller<'_>,
    settings: &Effective,
    space: &AgentSpace<'_>,
    agent: &TrackedAgent,
    (records, manual): (&[PathBuf], bool),
    command: &[OsString],
) -> Result<Option<i32>> {
    let current = current(settings);
    let Some(run) = AgentRun::of(&current) else {
        return Ok(None);
    };
    let origin = groups::beside(caller.paths, &current, &space.repo, space.directory).await?;
    let beside = current.beside(origin.as_ref());
    let worker = agent_worker(
        caller,
        &current,
        space.owner,
        agent,
        (records, manual),
        command,
    )?;
    let mut env = place_env(caller.paths)?;
    if let Some(entry) = env::queue_entry() {
        env.push((env::QUEUE_ENTRY, entry.to_string()));
    }
    let start = Start {
        state: &caller.paths.state,
        origin: beside.origin,
        cwd: space.directory,
        label: space.label,
        focus: true,
        split: beside.split,
        argv: &worker,
        env: &env,
    };
    let left_running = match run {
        AgentRun::Session(attaches) => {
            let start = Start {
                label: &space.session,
                ..start
            };
            let session = attaches.run_agent(&start, space.owner, &agent.name).await?;
            report_detach(attaches, &space.number.to_string(), &session).await?
        }
        AgentRun::Place(places) => {
            let place = places.open(&start).await?;
            places.show(&place, space.label).await?;
            left_open(&place).await
        }
    };
    if left_running {
        let id = space.owner.id();
        crate::history::record_attach(caller.paths, &SpaceSelector::id(id)).await;
    }
    Ok(Some(0))
}

/// The worker that runs `agent`'s tracked `command` for `owner` in a place
/// `host` opens, closing a workspace's with the workspace when config says
/// so; a chat's goes with the chat's removal.
fn agent_worker(
    caller: Caller<'_>,
    host: &Configured,
    owner: Owner<'_>,
    agent: &TrackedAgent,
    (records, manual): (&[PathBuf], bool),
    command: &[OsString],
) -> Result<Vec<OsString>> {
    let name = host.host.name();
    let closes = host.closes_when_done && matches!(owner, Owner::Workspace(_));
    crate::workers::internal_command(
        caller.paths,
        false,
        crate::workers::Worker::Session {
            space: owner.id(),
            agent,
            records,
            command,
            host: name,
            close_in: closes.then_some(name),
            manual,
        },
    )
}

/// Whether the host under `settings` opens places beside this terminal for
/// the starts made from it.
pub fn opens_beside(caller: Caller<'_>, settings: &Effective) -> bool {
    caller.launcher().host(settings).places().is_some()
}

#[cfg(test)]
mod tests;
