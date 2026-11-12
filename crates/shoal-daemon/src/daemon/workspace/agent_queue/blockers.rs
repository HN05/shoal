//! What blocks a queued launch. A launch starts once every workspace blocking
//! it is done at its current HEAD, every chat blocking it is done, either
//! removed, every launch has opened its
//! workspace, which then stands in for it, every issue has closed or is
//! done in a workspace of the repository that links it, and the existing
//! workspace it opens, if its setup had failed, has passed a post-setup hook
//! since. It builds on the branch of the done blocker that contains the
//! others.
use crate::client::repository_ids::RepositoryId;
use crate::client::{queue::QueueEntryId, space_ids::SpaceId, space_names::SpaceSelector};
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    daemon::{
        blockers,
        links::pr::current_head,
        store::{self, WORKSPACE_COLUMNS},
        workspace::Manager,
    },
    git,
    model::{Blocker, SpaceKind, Workspace},
    state::{BlockerKind, QueueKind, QueueState},
};

/// Whether a waiting launch's blockers let it start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Readiness {
    /// It may start, on the branch of a done blocker that is not merged.
    Ready { base: Option<String> },
    /// What still blocks it, by name or URL.
    Waiting(Vec<String>),
}

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
    branch: String,
    path: std::path::PathBuf,
    head: String,
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
pub(super) fn record(
    db: &Connection,
    entry: &QueueEntryId,
    repository: &RepositoryId,
    blockers: &[Blocker],
) -> Result<()> {
    for blocker in blockers {
        let (kind, target) = match blocker {
            Blocker::Issue(url) => (BlockerKind::Issue, url.to_string()),
            Blocker::Pr(url) => (BlockerKind::Pr, url.to_string()),
            Blocker::CiRun(url) => (BlockerKind::CiRun, url.to_string()),
            Blocker::Space(selector) => space(db, Some(entry), repository, selector)?,
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
    repository: &RepositoryId,
    blockers: &[Blocker],
) -> Result<()> {
    for blocker in blockers {
        if let Blocker::Space(selector) = blocker {
            space(db, None, repository, selector)?;
        }
    }
    Ok(())
}

/// The space or queued launch of `repository`, other than `entry`, that
/// `selector` names. A launch that already opened its workspace stands for
/// that workspace.
fn space(
    db: &Connection,
    entry: Option<&QueueEntryId>,
    repository: &RepositoryId,
    selector: &str,
) -> Result<(BlockerKind, String)> {
    let selected = match SpaceSelector::parse(selector) {
        Ok(selector) => crate::daemon::spaces::selected(db, &selector)?,
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
    let query = match kind {
        SpaceKind::Workspace => "SELECT 1 FROM workspaces WHERE id=?1 AND repository_id=?2",
        SpaceKind::Chat => "SELECT 1 FROM chats WHERE id=?1 AND repository_id=?2",
    };
    crate::daemon::store::exists(db, query, params![id, repository])
}

/// Hold back `entry` until a preparation of `workspace`, the one it opens,
/// whose setup failed, gets past its post-setup hook.
pub(super) fn wait_for_setup(
    db: &Connection,
    entry: &QueueEntryId,
    workspace: &SpaceId,
) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO agent_queue_blockers(entry_id,kind,target) VALUES (?1,?2,?3)",
        params![entry, BlockerKind::Setup, workspace],
    )?;
    Ok(())
}

/// Let `workspace`, which launch `entry` opened, stand for the launch.
pub(super) fn replace_launch(
    db: &Connection,
    entry: &QueueEntryId,
    workspace: &SpaceId,
) -> Result<()> {
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

impl Manager {
    /// Refuse `blockers` when they name work of `repository` that a queued
    /// launch could not wait for.
    pub async fn check_blockers(
        &self,
        repository: &crate::repository::Selector,
        blockers: Vec<Blocker>,
    ) -> Result<()> {
        let repository = self.repository(repository).await?.id;
        self.store
            .run(move |db| ensure_spaces_exist(db, &repository, &blockers))
            .await
    }

    /// The readiness of each waiting launch with blockers, by entry ID.
    pub(super) async fn blocker_readiness(&self) -> Result<HashMap<QueueEntryId, Readiness>> {
        let recorded = self.store.run(waiting_blockers).await?;
        let mut readiness = HashMap::new();
        for (entry, progress) in self.entry_progress(recorded).await? {
            readiness.insert(entry, readiness_of(progress).await?);
        }
        Ok(readiness)
    }

    /// Let the launches waiting for the setup of `workspace` go, once a
    /// preparation there got past its post-setup hook.
    pub(crate) async fn release_setup_waits(&self, workspace: &SpaceId) -> Result<()> {
        let workspace = workspace.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "DELETE FROM agent_queue_blockers WHERE kind=?1 AND target=?2",
                    params![BlockerKind::Setup, workspace],
                )?;
                Ok(())
            })
            .await?;
        self.agent_queue.wake();
        Ok(())
    }

    /// The progress of each entry's blockers, looking each one up once.
    async fn entry_progress(
        &self,
        recorded: Vec<(QueueEntryId, Recorded)>,
    ) -> Result<HashMap<QueueEntryId, Vec<Progress>>> {
        let mut known = HashMap::new();
        let mut entries: HashMap<QueueEntryId, Vec<Progress>> = HashMap::new();
        for (entry, blocker) in recorded {
            let progress = match known.get(&blocker) {
                Some(progress) => progress,
                None => {
                    let found = self.progress(&blocker).await?;
                    known.entry(blocker).or_insert(found)
                }
            };
            entries.entry(entry).or_default().push(progress.clone());
        }
        Ok(entries)
    }

    async fn progress(&self, blocker: &Recorded) -> Result<Progress> {
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
                    .run(move |db| workspace_by_id(db, &query))
                    .await?
                {
                    Some(workspace) => self.workspace_progress(workspace).await,
                    None => self.chat_progress(&id).await,
                }
            }
            // The row goes once the setup is done.
            BlockerKind::Setup => {
                let workspace = SpaceId::from(target);
                let name = self
                    .store
                    .run(move |db| workspace_by_id(db, &workspace))
                    .await?
                    .map(|workspace| workspace.name);
                Ok(name.map_or(Progress::Complete, |name| {
                    Progress::Waiting(format!("setup of {name}"))
                }))
            }
            BlockerKind::Issue => {
                let url = target.clone();
                let linked = match repository {
                    Some(repository) => {
                        (self.store)
                            .run(move |db| issue_workspaces(db, &repository, &url))
                            .await?
                    }
                    None => Vec::new(),
                };
                self.issue_progress(target, linked).await
            }
            // Only the forge says when these clear; its poll forgets them then.
            BlockerKind::Pr | BlockerKind::CiRun => Ok(Progress::Waiting(target)),
        }
    }

    /// An open issue is done once any workspace that links it is done; until
    /// then it waits under those workspaces' names, or its URL without one.
    async fn issue_progress(&self, url: String, linked: Vec<Workspace>) -> Result<Progress> {
        let mut waiting = Vec::new();
        for workspace in linked {
            match self.workspace_progress(workspace).await? {
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
    async fn chat_progress(&self, id: &SpaceId) -> Result<Progress> {
        Ok(match self.chat_by_id(id).await? {
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
    pub(crate) async fn cleared_locally(
        &self,
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
        Ok(match self.progress(&recorded).await? {
            Progress::Complete => Some("it is done or removed".into()),
            Progress::Done(done) => Some(format!("{} is done", done.name)),
            Progress::Waiting(_) => None,
        })
    }

    /// A workspace is done while its completion holds at its current HEAD.
    async fn workspace_progress(&self, workspace: Workspace) -> Result<Progress> {
        // Only a workspace on a branch records completions.
        let (Some(completion), Some(branch)) = (
            self.completion(&workspace.id).await?,
            workspace.branch.clone(),
        ) else {
            return Ok(Progress::Waiting(workspace.name.into()));
        };
        match current_head(&workspace).await {
            Ok(head) if head == completion.head => Ok(Progress::Done(Done {
                name: workspace.name.into(),
                branch,
                path: workspace.path,
                head,
            })),
            _ => Ok(Progress::Waiting(workspace.name.into())),
        }
    }

    /// The issues, PRs and CI runs queued launches wait for.
    pub(crate) async fn queued_forge_blockers(&self) -> Result<HashSet<blockers::Target>> {
        self.store.run(forge_blockers).await
    }

    /// Forget the queued launches' blockers that `lookups` says cleared.
    pub(crate) async fn forget_cleared_launch_blockers(
        &self,
        lookups: &blockers::Lookups,
    ) -> Result<()> {
        let cleared: Vec<blockers::Target> = (lookups.iter())
            .filter(|(_, lookup)| matches!(lookup, blockers::Lookup::Cleared(_)))
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

/// The name of queue entry `id`, while it is queued or starting.
fn launch_name(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT name FROM agent_queue WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?)
}

fn workspace_by_id(db: &Connection, id: &SpaceId) -> Result<Option<Workspace>> {
    recorded_workspace(db, "id=?1", [id])
}

/// The workspaces of `repository` that link the issue at `url`, by name.
fn issue_workspaces(
    db: &Connection,
    repository: &RepositoryId,
    url: &str,
) -> Result<Vec<Workspace>> {
    Ok(db
        .prepare(&format!(
            "SELECT {WORKSPACE_COLUMNS} FROM workspaces WHERE repository_id=?1
             AND id IN (SELECT space_id FROM space_issue WHERE url=?2) ORDER BY name"
        ))?
        .query_map(params![repository, url], store::workspace)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn recorded_workspace<P: rusqlite::Params>(
    db: &Connection,
    condition: &str,
    params: P,
) -> Result<Option<Workspace>> {
    Ok(db
        .query_row(
            &format!("SELECT {WORKSPACE_COLUMNS} FROM workspaces WHERE {condition}"),
            params,
            store::workspace,
        )
        .optional()?)
}

/// The issues, PRs and CI runs queued launches wait for.
fn forge_blockers(db: &mut Connection) -> Result<HashSet<blockers::Target>> {
    Ok(db
        .prepare("SELECT DISTINCT kind,target FROM agent_queue_blockers WHERE kind IN (?1,?2,?3)")?
        .query_map(
            params![BlockerKind::Issue, BlockerKind::Pr, BlockerKind::CiRun],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<rusqlite::Result<_>>()?)
}

fn forget_blockers(db: &mut Connection, cleared: &[blockers::Target]) -> Result<()> {
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
                base: Some(candidate.branch.clone()),
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
            &other.head,
            &candidate.head,
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

#[cfg(test)]
mod tests;
