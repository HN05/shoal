//! What blocks a queued launch, and when each blocker clears.
//! `docs/design/queue.md` gives the rules.
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::progress::Blocker;
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::{BranchName, CommitId};
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
pub use shoal_daemon_base::blockers::{Lookup, Lookups, Readiness, Target};

use crate::Agents;
use crate::daemon::{Daemon, Records};
use crate::git;
use crate::model::SpaceKind;
use crate::model::Workspace;
use crate::state::BlockerKind;
use crate::state::QueueKind;
use crate::state::QueueState;

/// What Shoal tells an agent it stops because its space is blocked, which
/// waits for the agent's restore and so starts its turn there.
pub const STOPPED_BLOCKED: &str =
    "Shoal stopped you while your work was blocked and restored you to continue it.";

/// One blocker as admission sees it.
#[derive(Debug, Clone)]
enum Progress {
    Complete,
    /// Done at its current HEAD, which a launch can build on.
    Done(Done),
    Waiting(String),
}

#[derive(Debug, Clone)]
struct Done {
    name: String,
    branch: BranchName,
    path: std::path::PathBuf,
    head: CommitId,
}

/// A recorded blocker of a waiting launch or a space in `repository`; a
/// chat without one has only space and forge blockers that clear.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Recorded {
    repository: Option<RepositoryId>,
    kind: BlockerKind,
    target: String,
}

/// Record `blockers` as what blocks `entry`, a launch in `repository`.
pub fn record(
    db: &Connection,
    records: &dyn Records,
    entry: &QueueEntryId,
    repository: &RepositoryId,
    blockers: &[Blocker],
) -> Result<()> {
    for blocker in blockers {
        let (kind, target) = match blocker {
            Blocker::Issue(url) => (BlockerKind::Issue, url.to_string()),
            Blocker::Pr(url) => (BlockerKind::Pr, url.to_string()),
            Blocker::CiRun(url) => (BlockerKind::CiRun, url.to_string()),
            Blocker::Space(selector) => space(db, records, Some(entry), repository, selector)?,
        };
        db.execute(
            "INSERT OR IGNORE INTO agent_queue_blockers(entry_id,kind,target) VALUES (?1,?2,?3)",
            params![entry, kind, target],
        )?;
    }
    Ok(())
}

/// Refuse the workspaces and queued launches of `blockers` that `repository`
/// lacks.
fn ensure_spaces_exist(
    db: &Connection,
    records: &dyn Records,
    repository: &RepositoryId,
    blockers: &[Blocker],
) -> Result<()> {
    for blocker in blockers {
        if let Blocker::Space(selector) = blocker {
            space(db, records, None, repository, selector)?;
        }
    }
    Ok(())
}

/// The space or queued launch of `repository`, other than `entry`, that
/// `selector` names. A launch that already opened its workspace stands for
/// that workspace.
fn space(
    db: &Connection,
    records: &dyn Records,
    entry: Option<&QueueEntryId>,
    repository: &RepositoryId,
    selector: &str,
) -> Result<(BlockerKind, String)> {
    let selected = match SpaceSelector::parse(selector) {
        Ok(selector) => records.selected_space(db, &selector)?,
        Err(_) => None,
    };
    if let Some((kind, id)) = selected
        && space_of(db, kind, &id, repository)?
    {
        return Ok((BlockerKind::Space, id.into()));
    }
    let launch: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT id,COALESCE(space_id,opens_workspace_id) FROM agent_queue
             WHERE repository_id=?1 AND kind=?3 AND id=?2 AND id IS NOT ?4",
            params![repository, selector, QueueKind::Launch, entry],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match launch {
        Some((_, Some(workspace))) => Ok((BlockerKind::Space, workspace)),
        Some((id, None)) => Ok((BlockerKind::Launch, id)),
        None => bail!("{selector} is neither a space nor a queued agent of this repository"),
    }
}

/// Whether the `kind` space `id` belongs to `repository`.
fn space_of(
    db: &Connection,
    kind: SpaceKind,
    id: &SpaceId,
    repository: &RepositoryId,
) -> Result<bool> {
    Ok(shoal_store::spaces::facts(db, id)?
        .is_some_and(|space| space.kind == kind && space.repository.as_ref() == Some(repository)))
}

/// Hold back `entry` until a preparation of `workspace`, the one it opens,
/// whose setup failed, gets past its post-setup hook.
pub fn wait_for_setup(db: &Connection, entry: &QueueEntryId, workspace: &SpaceId) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO agent_queue_blockers(entry_id,kind,target) VALUES (?1,?2,?3)",
        params![entry, BlockerKind::Setup, workspace],
    )?;
    Ok(())
}

/// Let `workspace`, which launch `entry` opened, stand for the launch.
pub fn replace_launch(db: &Connection, entry: &QueueEntryId, workspace: &SpaceId) -> Result<()> {
    db.execute(
        "UPDATE OR IGNORE agent_queue_blockers SET kind=?3,target=?2 WHERE kind=?4 AND target=?1",
        params![entry, workspace, BlockerKind::Space, BlockerKind::Launch],
    )?;
    db.execute(
        "DELETE FROM agent_queue_blockers WHERE kind=?2 AND target=?1",
        params![entry, BlockerKind::Launch],
    )?;
    Ok(())
}

impl Agents {
    /// Refuse `blockers` when they name work of `repository` that a queued
    /// launch could not wait for.
    pub async fn check_blockers(
        &self,
        daemon: &dyn Daemon,
        repository: &crate::repository::Selector,
        blockers: Vec<Blocker>,
    ) -> Result<()> {
        let repository = daemon.selected_repository(repository).await?.id;
        let records = self.records.clone();
        self.store
            .run(move |db| ensure_spaces_exist(db, &*records, &repository, &blockers))
            .await
    }

    /// The readiness of each waiting launch with blockers, by entry ID.
    pub async fn blocker_readiness(
        &self,
        daemon: &dyn Daemon,
    ) -> Result<HashMap<QueueEntryId, Readiness>> {
        let recorded = self.store.run(waiting_blockers).await?;
        let mut readiness = HashMap::new();
        for (entry, progress) in self.entry_progress(daemon, recorded).await? {
            readiness.insert(entry, readiness_of(progress).await?);
        }
        Ok(readiness)
    }

    /// The readiness of the launch waiting to open `workspace` by its
    /// blockers besides the workspace's setup, which the setup's own
    /// preparation waits for; `None` when no launch waits there.
    pub async fn opening_readiness(
        &self,
        daemon: &dyn Daemon,
        workspace: &SpaceId,
    ) -> Result<Option<Readiness>> {
        let workspace = workspace.to_owned();
        let recorded = (self.store)
            .run(move |db| opening_blockers(db, &workspace))
            .await?;
        let Some(recorded) = recorded else {
            return Ok(None);
        };
        let mut progress = Vec::new();
        for blocker in &recorded {
            progress.push(self.progress(daemon, blocker).await?);
        }
        readiness_of(progress).await.map(Some)
    }

    /// Let the launches waiting for the setup of `workspace` go, once a
    /// preparation there got past its post-setup hook, those whose start
    /// asked for it at once.
    pub async fn release_setup_waits(
        &self,
        daemon: &dyn Daemon,
        workspace: &SpaceId,
    ) -> Result<()> {
        let released = workspace.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "DELETE FROM agent_queue_blockers WHERE kind=?1 AND target=?2",
                    params![BlockerKind::Setup, released],
                )?;
                Ok(())
            })
            .await?;
        self.start_released_at_once(daemon, workspace).await?;
        self.agent_queue.wake();
        Ok(())
    }

    /// The progress of each entry's blockers, looking each one up once.
    async fn entry_progress(
        &self,
        daemon: &dyn Daemon,
        recorded: Vec<(QueueEntryId, Recorded)>,
    ) -> Result<HashMap<QueueEntryId, Vec<Progress>>> {
        let mut known = HashMap::new();
        let mut entries: HashMap<QueueEntryId, Vec<Progress>> = HashMap::new();
        for (entry, blocker) in recorded {
            let progress = match known.get(&blocker) {
                Some(progress) => progress,
                None => {
                    let found = self.progress(daemon, &blocker).await?;
                    known.entry(blocker).or_insert(found)
                }
            };
            entries.entry(entry).or_default().push(progress.clone());
        }
        Ok(entries)
    }

    async fn progress(&self, daemon: &dyn Daemon, blocker: &Recorded) -> Result<Progress> {
        let Recorded {
            repository,
            kind,
            target,
        } = blocker.clone();
        match kind {
            BlockerKind::Launch => {
                let name = self.store.run(move |db| launch_name(db, &target)).await?;
                Ok(name.map_or(Progress::Complete, Progress::Waiting))
            }
            BlockerKind::Space => {
                let id = SpaceId::from(target);
                let query = id.clone();
                match self
                    .store
                    .run(move |db| shoal_store::spaces::find_workspace(db, &query))
                    .await?
                {
                    Some(workspace) => self.workspace_progress(daemon, workspace).await,
                    None => self.chat_progress(daemon, &id).await,
                }
            }
            // The row goes once the setup is done.
            BlockerKind::Setup => {
                let workspace = SpaceId::from(target);
                let name = self
                    .store
                    .run(move |db| shoal_store::spaces::find_workspace(db, &workspace))
                    .await?
                    .map(|workspace| workspace.name);
                Ok(name.map_or(Progress::Complete, |name| {
                    Progress::Waiting(format!("setup of {name}"))
                }))
            }
            BlockerKind::Issue => {
                let (url, records) = (target.clone(), self.records.clone());
                let linked = match repository {
                    Some(repository) => {
                        (self.store)
                            .run(move |db| records.issue_workspaces(db, &repository, &url))
                            .await?
                    }
                    None => Vec::new(),
                };
                self.issue_progress(daemon, target, linked).await
            }
            // Only the forge says when these clear; its poll forgets them then.
            BlockerKind::Pr | BlockerKind::CiRun => Ok(Progress::Waiting(target)),
        }
    }

    /// An open issue is done once any workspace that links it is done; until
    /// then it waits under those workspaces' names, or its URL without one.
    async fn issue_progress(
        &self,
        daemon: &dyn Daemon,
        url: String,
        linked: Vec<Workspace>,
    ) -> Result<Progress> {
        let mut waiting = Vec::new();
        for workspace in linked {
            match self.workspace_progress(daemon, workspace).await? {
                Progress::Waiting(name) => waiting.push(name),
                done => return Ok(done),
            }
        }
        Ok(Progress::Waiting(if waiting.is_empty() {
            url
        } else {
            waiting.join(" or ")
        }))
    }

    /// A chat is done once it records completion; one removed leaves
    /// nothing to wait for.
    async fn chat_progress(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<Progress> {
        Ok(match daemon.chat_by_id(id).await? {
            Some(chat) if chat.completion.is_some() => Progress::Complete,
            Some(chat) => Progress::Waiting(chat.name.into()),
            None => Progress::Complete,
        })
    }

    /// What cleared the space or issue blocker `target` of a space in
    /// `repository`, as admission decides for a launch: a space done at its
    /// current HEAD or removed, or an issue done in a workspace that links
    /// it; none while it blocks, or for the other kinds, which only the
    /// forge clears.
    pub async fn cleared_locally(
        &self,
        daemon: &dyn Daemon,
        repository: Option<&RepositoryId>,
        kind: BlockerKind,
        target: &str,
    ) -> Result<Option<String>> {
        if !matches!(kind, BlockerKind::Space | BlockerKind::Issue) {
            return Ok(None);
        }
        let recorded = Recorded {
            repository: repository.cloned(),
            kind,
            target: target.to_owned(),
        };
        Ok(match self.progress(daemon, &recorded).await? {
            Progress::Complete => Some("it is done or removed".into()),
            Progress::Done(done) => Some(format!("{} is done", done.name)),
            Progress::Waiting(_) => None,
        })
    }

    /// A workspace is done while its completion holds at its current HEAD.
    async fn workspace_progress(
        &self,
        daemon: &dyn Daemon,
        workspace: Workspace,
    ) -> Result<Progress> {
        // Only a workspace on a branch records completions.
        let (Some(completion), Some(branch)) = (
            daemon.completion(&workspace.id).await?,
            workspace.branch().cloned(),
        ) else {
            return Ok(Progress::Waiting(workspace.name.into()));
        };
        match daemon.current_head(&workspace).await {
            Ok(head) if head == completion.head => Ok(Progress::Done(Done {
                name: workspace.name.into(),
                branch,
                path: workspace.part.path,
                head,
            })),
            _ => Ok(Progress::Waiting(workspace.name.into())),
        }
    }

    /// The issues, PRs and CI runs queued launches wait for.
    pub async fn queued_forge_blockers(&self) -> Result<HashSet<Target>> {
        self.store.run(forge_blockers).await
    }

    /// Forget the queued launches' blockers that `lookups` says cleared.
    pub async fn forget_cleared_launch_blockers(&self, lookups: &Lookups) -> Result<()> {
        let cleared: Vec<Target> = (lookups.iter())
            .filter(|(_, lookup)| matches!(lookup, Lookup::Cleared(_)))
            .map(|(target, _)| target.clone())
            .collect();
        self.store
            .run(move |db| forget_blockers(db, &cleared))
            .await
    }
}

/// Each blocker of the waiting launches with its entry, in queue order.
fn waiting_blockers(db: &mut Connection) -> Result<Vec<(QueueEntryId, Recorded)>> {
    Ok(db
        .prepare(
            "SELECT d.entry_id,q.repository_id,d.kind,d.target
             FROM agent_queue_blockers d JOIN agent_queue q ON q.id=d.entry_id
             WHERE q.state=?1 ORDER BY q.seq,d.rowid",
        )?
        .query_map([QueueState::Queued], |row| {
            Ok((
                row.get::<_, QueueEntryId>(0)?,
                Recorded {
                    repository: Some(row.get(1)?),
                    kind: row.get(2)?,
                    target: row.get(3)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The blockers besides its setup of the launch waiting to open
/// `workspace`, if one does.
fn opening_blockers(db: &mut Connection, workspace: &SpaceId) -> Result<Option<Vec<Recorded>>> {
    let launch: Option<(QueueEntryId, RepositoryId)> = db
        .query_row(
            "SELECT id,repository_id FROM agent_queue
             WHERE opens_workspace_id=?1 AND kind=?2 AND state=?3",
            params![workspace, QueueKind::Launch, QueueState::Queued],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((entry, repository)) = launch else {
        return Ok(None);
    };
    let recorded = db
        .prepare(
            "SELECT kind,target FROM agent_queue_blockers WHERE entry_id=?1 AND kind IS NOT ?2
             ORDER BY rowid",
        )?
        .query_map(params![entry, BlockerKind::Setup], |row| {
            Ok(Recorded {
                repository: Some(repository.clone()),
                kind: row.get(0)?,
                target: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(recorded))
}

/// The name of queue entry `id`, while it is queued or starting.
fn launch_name(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT name FROM agent_queue WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?)
}

/// The issues, PRs and CI runs queued launches wait for.
fn forge_blockers(db: &mut Connection) -> Result<HashSet<Target>> {
    Ok(db
        .prepare("SELECT DISTINCT kind,target FROM agent_queue_blockers WHERE kind IN (?1,?2,?3)")?
        .query_map(
            params![BlockerKind::Issue, BlockerKind::Pr, BlockerKind::CiRun],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<rusqlite::Result<_>>()?)
}

fn forget_blockers(db: &mut Connection, cleared: &[Target]) -> Result<()> {
    for (kind, target) in cleared {
        db.execute(
            "DELETE FROM agent_queue_blockers WHERE kind=?1 AND target=?2",
            params![kind, target],
        )?;
    }
    Ok(())
}

/// Ready when nothing blocks, on the branch of the done blocker whose HEAD
/// contains every other's; done blockers on separate branches wait
/// until all but one have merged and gone.
async fn readiness_of(progress: Vec<Progress>) -> Result<Readiness> {
    let mut waiting = Vec::new();
    let mut done = Vec::new();
    for blocker in progress {
        match blocker {
            Progress::Complete => {}
            Progress::Done(found) => done.push(found),
            Progress::Waiting(label) => waiting.push(label),
        }
    }
    if !waiting.is_empty() {
        return Ok(Readiness::Waiting(waiting));
    }
    if done.is_empty() {
        return Ok(Readiness::Ready { base: None });
    }
    for candidate in &done {
        if contains_all(candidate, &done).await? {
            return Ok(Readiness::Ready {
                base: Some(candidate.branch.clone().into()),
            });
        }
    }
    Ok(Readiness::Waiting(
        done.into_iter().map(|done| done.name).collect(),
    ))
}

async fn contains_all(candidate: &Done, done: &[Done]) -> Result<bool> {
    for other in done {
        let contained = git::is_ancestor(
            &candidate.path,
            other.head.as_str(),
            candidate.head.as_str(),
            git::isolated_command,
        )
        .await
        .with_context(|| format!("compare {} with {}", candidate.name, other.name))?;
        if !contained {
            return Ok(false);
        }
    }
    Ok(true)
}
