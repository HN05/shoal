//! The work a queued launch waits for. A launch starts once every workspace
//! it depends on is done at its current HEAD or removed, every launch has
//! opened its workspace, which then stands in for it, and every issue has
//! closed or is done in a workspace of the repository that links it. It
//! builds on the branch of the done dependency that contains the others.
use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    daemon::{
        links::pr::current_head,
        store::{self, WORKSPACE_COLUMNS},
        workspace::Manager,
    },
    forge::{ForgeRepo, account::Account},
    git,
    model::{Dependency, Workspace},
    state::{DependencyKind, QueueKind, QueueState},
};

/// Whether a waiting launch's dependencies let it start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Readiness {
    /// It may start, on the branch of a done dependency that is not merged.
    Ready { base: Option<String> },
    /// What it still waits for, by name or URL.
    Waiting(Vec<String>),
}

/// One dependency as admission sees it.
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

/// A recorded dependency of a waiting launch in `repository`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Recorded {
    repository: String,
    kind: DependencyKind,
    target: String,
}

/// Record `after` as the dependencies of `entry`, a launch in `repository`.
pub(super) fn record(
    db: &Connection,
    entry: &str,
    repository: &str,
    after: &[Dependency],
) -> Result<()> {
    for dependency in after {
        let (kind, target) = match dependency {
            Dependency::Issue(url) => (DependencyKind::Issue, url.clone()),
            Dependency::Work(selector) => work(db, entry, repository, selector)?,
        };
        db.execute(
            "INSERT OR IGNORE INTO agent_queue_dependencies(entry_id,kind,target) VALUES (?1,?2,?3)",
            params![entry, kind, target],
        )?;
    }
    Ok(())
}

/// The workspace or queued launch of `repository` other than `entry` that
/// `selector` names. A launch that already opened its workspace stands for
/// that workspace.
fn work(
    db: &Connection,
    entry: &str,
    repository: &str,
    selector: &str,
) -> Result<(DependencyKind, String)> {
    let workspace: Option<String> = db
        .query_row(
            "SELECT id FROM workspaces WHERE repository_id=?1 AND (id=?2 OR name=?2)",
            params![repository, selector],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = workspace {
        return Ok((DependencyKind::Workspace, id));
    }
    let launch: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT id,COALESCE(workspace_id,opens_workspace_id) FROM agent_queue
             WHERE repository_id=?1 AND kind=?3 AND (id=?2 OR name=?2) AND id<>?4",
            params![repository, selector, QueueKind::Launch, entry],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match launch {
        Some((_, Some(workspace))) => Ok((DependencyKind::Workspace, workspace)),
        Some((id, None)) => Ok((DependencyKind::Launch, id)),
        None => bail!("{selector} is neither a workspace nor a queued agent of this repository"),
    }
}

/// Let `workspace`, which launch `entry` opened, stand for the launch.
pub(super) fn replace_launch(db: &Connection, entry: &str, workspace: &str) -> Result<()> {
    db.execute(
        "UPDATE OR IGNORE agent_queue_dependencies SET kind=?3,target=?2 WHERE kind=?4 AND target=?1",
        params![
            entry,
            workspace,
            DependencyKind::Workspace,
            DependencyKind::Launch
        ],
    )?;
    db.execute(
        "DELETE FROM agent_queue_dependencies WHERE kind=?2 AND target=?1",
        params![entry, DependencyKind::Launch],
    )?;
    Ok(())
}

impl Manager {
    /// The readiness of each waiting launch with dependencies, by entry ID.
    pub(super) async fn dependency_readiness(&self) -> Result<HashMap<String, Readiness>> {
        let recorded = self.store.run(waiting_dependencies).await?;
        let mut readiness = HashMap::new();
        for (entry, progress) in self.entry_progress(recorded).await? {
            readiness.insert(entry, readiness_of(progress).await?);
        }
        Ok(readiness)
    }

    /// The progress of each entry's dependencies, looking each one up once.
    async fn entry_progress(
        &self,
        recorded: Vec<(String, Recorded)>,
    ) -> Result<HashMap<String, Vec<Progress>>> {
        let mut known = HashMap::new();
        let mut entries: HashMap<String, Vec<Progress>> = HashMap::new();
        for (entry, dependency) in recorded {
            let progress = match known.get(&dependency) {
                Some(progress) => progress,
                None => {
                    let found = self.progress(&dependency).await?;
                    known.entry(dependency).or_insert(found)
                }
            };
            entries.entry(entry).or_default().push(progress.clone());
        }
        Ok(entries)
    }

    async fn progress(&self, dependency: &Recorded) -> Result<Progress> {
        let Recorded {
            repository,
            kind,
            target,
        } = dependency.clone();
        match kind {
            DependencyKind::Launch => {
                let name = self.store.run(move |db| launch_name(db, &target)).await?;
                Ok(name.map_or(Progress::Complete, Progress::Waiting))
            }
            DependencyKind::Workspace => {
                match self
                    .store
                    .run(move |db| workspace_by_id(db, &target))
                    .await?
                {
                    Some(workspace) => self.workspace_progress(workspace).await,
                    None => Ok(Progress::Complete),
                }
            }
            DependencyKind::Issue => {
                let url = target.clone();
                let linked = self
                    .store
                    .run(move |db| issue_workspace(db, &repository, &url))
                    .await?;
                match linked {
                    Some(workspace) => self.workspace_progress(workspace).await,
                    None => Ok(Progress::Waiting(target)),
                }
            }
        }
    }

    /// A workspace is done while its completion holds at its current HEAD.
    async fn workspace_progress(&self, workspace: Workspace) -> Result<Progress> {
        let Some(completion) = self.completion(&workspace.id).await? else {
            return Ok(Progress::Waiting(workspace.name));
        };
        match current_head(&workspace).await {
            Ok(head) if head == completion.head => Ok(Progress::Done(Done {
                name: workspace.name,
                branch: workspace.branch,
                path: workspace.path,
                head,
            })),
            _ => Ok(Progress::Waiting(workspace.name)),
        }
    }

    /// Forget the issue dependencies that have closed.
    pub(crate) async fn poll_dependency_issues(&self) -> Result<()> {
        let urls = self.store.run(issue_dependencies).await?;
        let account = self.user_account();
        let mut failure = None;
        for url in urls {
            match issue_closed(&account, &url).await {
                Ok(true) => self.forget_issue(url).await?,
                Ok(false) => {}
                Err(error) => {
                    failure.get_or_insert(error.context(format!("look up issue {url}")));
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    async fn forget_issue(&self, url: String) -> Result<()> {
        self.store
            .run(move |db| forget_issue_dependency(db, &url))
            .await
    }
}

async fn issue_closed(account: &Account, url: &str) -> Result<bool> {
    let forge = ForgeRepo::from_issue_url(url)?;
    let (number, _) = forge.issue(url)?;
    forge.issue_closed(account, number).await
}

/// Each dependency of the waiting launches with its entry, in queue order.
fn waiting_dependencies(db: &mut Connection) -> Result<Vec<(String, Recorded)>> {
    Ok(db
        .prepare(
            "SELECT d.entry_id,q.repository_id,d.kind,d.target
             FROM agent_queue_dependencies d JOIN agent_queue q ON q.id=d.entry_id
             WHERE q.state=?1 ORDER BY q.seq,d.rowid",
        )?
        .query_map([QueueState::Queued], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Recorded {
                    repository: row.get(1)?,
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

fn workspace_by_id(db: &Connection, id: &str) -> Result<Option<Workspace>> {
    recorded_workspace(db, "id=?1", [id])
}

/// The workspace of `repository` that links the issue at `url`.
fn issue_workspace(db: &Connection, repository: &str, url: &str) -> Result<Option<Workspace>> {
    recorded_workspace(
        db,
        "repository_id=?1 AND id IN (SELECT workspace_id FROM workspace_issue WHERE url=?2)",
        [repository, url],
    )
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

/// The URLs of the issues queued launches wait for.
fn issue_dependencies(db: &mut Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT DISTINCT target FROM agent_queue_dependencies WHERE kind=?1")?
        .query_map([DependencyKind::Issue], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?)
}

fn forget_issue_dependency(db: &Connection, url: &str) -> Result<()> {
    db.execute(
        "DELETE FROM agent_queue_dependencies WHERE kind=?1 AND target=?2",
        params![DependencyKind::Issue, url],
    )?;
    Ok(())
}

/// Ready when nothing is waiting, on the branch of the done dependency whose
/// HEAD contains every other's; done dependencies on separate branches wait
/// until all but one have merged and gone.
async fn readiness_of(progress: Vec<Progress>) -> Result<Readiness> {
    let mut waiting = Vec::new();
    let mut done = Vec::new();
    for dependency in progress {
        match dependency {
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
