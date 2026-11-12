//! What waits on what, from the waiting work to the work it waits on:
//! queued launches on their blockers.
use anyhow::Result;
use rusqlite::{Connection, params};

use super::super::rank::{Edge, Node};
use crate::client::{queue::QueueEntryId, space_ids::SpaceId};
use crate::{
    daemon::store,
    model::{Dependency, QueuedJob},
    state::{BlockerKind, QueueKind, QueueState},
};

/// Every edge.
pub(super) fn waits(db: &Connection) -> Result<Vec<Edge>> {
    let blocking = Blocking::read(db)?;
    queue_blockers(db, &blocking)
}

/// What a blocker stands for in the graph: its space, its queued launch, the
/// spaces working on its issue or owning its PR, or the queued jobs of its
/// CI run and of that run's workflow on the default branch.
struct Blocking {
    /// The waiting jobs' entries.
    jobs: Vec<(QueueEntryId, QueuedJob)>,
}

impl Blocking {
    fn read(db: &Connection) -> Result<Self> {
        let rows = db
            .prepare(
                "SELECT id,job FROM agent_queue WHERE kind=?1 AND state=?2 AND job IS NOT NULL",
            )?
            .query_map(params![QueueKind::Job, QueueState::Queued], |row| {
                Ok((row.get::<_, QueueEntryId>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut jobs = Vec::new();
        for (id, job) in rows {
            jobs.push((id, serde_json::from_str(&job)?));
        }
        Ok(Self { jobs })
    }

    /// The nodes the blocker `target` of `kind` stands for.
    fn nodes(&self, db: &Connection, kind: BlockerKind, target: &str) -> Result<Vec<Node>> {
        Ok(match kind {
            BlockerKind::Space => vec![Node::Space(target.to_owned().into())],
            BlockerKind::Launch => vec![Node::Entry(target.to_owned().into())],
            BlockerKind::Issue => (issue_spaces(db, target)?.into_iter())
                .map(Node::Space)
                .collect(),
            BlockerKind::Pr => (pr_spaces(db, target)?.into_iter())
                .map(Node::Space)
                .collect(),
            BlockerKind::CiRun => self.run_jobs(target),
            // The launch waits on its own workspace's setup, no other work.
            BlockerKind::Setup => Vec::new(),
        })
    }

    /// The waiting jobs of the run at `url`, and those on the default branch
    /// of its workflow when a job of it shows which that is.
    fn run_jobs(&self, url: &str) -> Vec<Node> {
        let workflow = (self.jobs.iter())
            .find(|(_, job)| job.run.url == url)
            .and_then(|(_, job)| job.run.workflow.as_deref());
        (self.jobs.iter())
            .filter(|(_, job)| {
                job.run.url == url
                    || (job.default_branch
                        && workflow
                            .is_some_and(|workflow| job.run.workflow.as_deref() == Some(workflow)))
            })
            .map(|(id, _)| Node::Entry(id.clone()))
            .collect()
    }

    /// An edge for each node `target` of `kind` stands for, from `waiting`.
    fn edges(
        &self,
        db: &Connection,
        waiting: &Node,
        kind: BlockerKind,
        target: &str,
    ) -> Result<Vec<Edge>> {
        Ok((self.nodes(db, kind, target)?.into_iter())
            .map(|on| Edge {
                waiting: waiting.clone(),
                on,
                dependency: Dependency::Blocked,
            })
            .collect())
    }
}

/// The spaces that link the issue at `url`.
fn issue_spaces(db: &Connection, url: &str) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare("SELECT space_id FROM space_issue WHERE url=?1")?
        .query_map([url], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The spaces assigned the PR at `url`, or that opened it.
fn pr_spaces(db: &Connection, url: &str) -> Result<Vec<SpaceId>> {
    let ids: Vec<SpaceId> = db
        .prepare("SELECT space_id FROM pr_cleanup UNION SELECT space_id FROM space_items WHERE kind='pr'")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut spaces = Vec::new();
    for id in ids {
        let links = store::space_links(db, &id)?;
        let opened = links.opened.iter().map(|item| &item.url);
        if links.prs.iter().chain(opened).any(|linked| linked == url) {
            spaces.push(id);
        }
    }
    Ok(spaces)
}

/// Queued launches on what blocks them, from their space once they opened
/// one.
fn queue_blockers(db: &Connection, blocking: &Blocking) -> Result<Vec<Edge>> {
    let rows = db
        .prepare(
            "SELECT b.entry_id,COALESCE(q.workspace_id,q.opens_workspace_id),b.kind,b.target
             FROM agent_queue_blockers b JOIN agent_queue q ON q.id=b.entry_id
             WHERE q.state=?1",
        )?
        .query_map([QueueState::Queued], |row| {
            Ok((
                row.get::<_, QueueEntryId>(0)?,
                row.get::<_, Option<SpaceId>>(1)?,
                row.get::<_, BlockerKind>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut edges = Vec::new();
    for (entry, space, kind, target) in rows {
        let waiting = space.map_or(Node::Entry(entry), Node::Space);
        edges.extend(blocking.edges(db, &waiting, kind, &target)?);
    }
    Ok(edges)
}
