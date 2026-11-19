//! A new workspace's preparation that waits before it creates the worktree:
//! for the blockers of the launch waiting to open the workspace, on whose
//! done branch the worktree is then created, and for disk space while disk
//! protection holds back new starts. The daemon records it with the
//! workspace, says what it waits for, and starts it once that allows, in
//! the scope of the space that started the workspace.
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::BranchName;

use crate::{
    daemon::{
        agent_queue::Readiness, log, manager::Manager, scope::Caller, workspace::build_pending_in,
    },
    model::{PreparationWait, Workspace},
    protocol::operations::{Failure, OperationSpec, Preparation},
    state::{QueueKind, QueueState},
};

/// A new workspace's preparation as its start leaves it: the `steps` it
/// runs in `env`, for the agent of a launch waiting to open the workspace
/// when `for_agent`.
pub(super) struct PreparationStart {
    pub steps: Preparation,
    pub env: Option<BTreeMap<String, String>>,
    pub for_agent: bool,
}

/// Whether a waiting preparation may start.
enum Gate {
    /// It may, creating the worktree on `base`, the branch of a done
    /// blocker, when one leaves it.
    Open {
        base: Option<String>,
    },
    Closed(PreparationWait),
}

/// A recorded waiting preparation.
struct Waiting {
    workspace: SpaceId,
    for_agent: bool,
    waits: PreparationWait,
}

/// A waiting preparation claimed to start: what it runs, in what
/// environment, and the space in whose scope it runs.
struct Claimed {
    steps: Preparation,
    env: Option<BTreeMap<String, String>>,
    scope: Option<SpaceId>,
}

impl Manager {
    /// Hold back the preparation `start` of the new `workspace`, which
    /// `caller` started, while what it waits for does not allow it,
    /// recording it waiting and returning whether it does. One that may
    /// start creates its worktree on the branch of a done blocker that
    /// leaves one.
    pub(super) async fn hold_preparation(
        &self,
        workspace: &Workspace,
        start: &PreparationStart,
        caller: Option<&Caller>,
    ) -> Result<bool> {
        match self
            .preparation_gate(&workspace.id, start.for_agent)
            .await?
        {
            Gate::Open { base } => {
                if let Some(base) = base {
                    self.build_pending_on(&workspace.id, base.into()).await?;
                }
                Ok(false)
            }
            Gate::Closed(waits) => {
                let scope = caller.map(|caller| caller.space_id.clone());
                (self.record_waiting_preparation(&workspace.id, start, scope, &waits)).await?;
                Ok(true)
            }
        }
    }

    /// Start each waiting preparation that what it waits for now allows,
    /// and record what each other one waits for now. None starts while an
    /// exit holds back new work.
    pub(crate) async fn start_waiting_preparations(self: &Arc<Self>) -> Result<()> {
        if self.gates.exiting().is_some() {
            return Ok(());
        }
        for waiting in self.store.run(waiting_preparations).await? {
            let workspace = waiting.workspace.clone();
            if let Err(error) = self.advance_waiting(waiting).await {
                log!("the waiting preparation of {workspace} did not advance: {error:#}");
            }
        }
        Ok(())
    }

    /// Refuse an explicit preparation of the workspace `spec` prepares, as
    /// `shoal restore` starts one, while that workspace's own preparation
    /// waits for work or disk space, which starts it once that clears; one
    /// that waits for its stopped agent waits no longer.
    pub(crate) async fn admit_explicit_preparation(&self, spec: &OperationSpec) -> Result<()> {
        let OperationSpec::Prepare { workspace, .. } = spec else {
            return Ok(());
        };
        let workspace = self.workspace(workspace).await?;
        match &workspace.preparation_waits {
            None => Ok(()),
            Some(PreparationWait::Stopped) => {
                let id = workspace.id.clone();
                (self.store).run(move |db| end_wait(db, &id)).await
            }
            Some(waits) => bail!(
                "the preparation of {} waits for {waits} and starts once that clears; `shoal rm {}` removes the workspace",
                workspace.name,
                workspace.number
            ),
        }
    }

    /// Whether the preparation of the workspace `id`, for the agent of a
    /// launch waiting to open it when `for_agent`, may start: once that
    /// launch's blockers are done and disk space allows. One whose launch
    /// was cancelled waits for an explicit preparation.
    async fn preparation_gate(&self, id: &SpaceId, for_agent: bool) -> Result<Gate> {
        let base = match self.opening_readiness(id).await? {
            Some(Readiness::Waiting(blocked_by)) => {
                return Ok(Gate::Closed(PreparationWait::Blockers { blocked_by }));
            }
            Some(Readiness::Ready { base }) => base,
            None if for_agent => return Ok(Gate::Closed(PreparationWait::Stopped)),
            None => None,
        };
        match self.disk_holds_back_starts().await? {
            true => Ok(Gate::Closed(PreparationWait::Disk)),
            false => Ok(Gate::Open { base }),
        }
    }

    /// Start the `waiting` preparation once it may start, or else record
    /// what it waits for now when that changed.
    async fn advance_waiting(self: &Arc<Self>, waiting: Waiting) -> Result<()> {
        let Waiting {
            workspace,
            for_agent,
            waits,
        } = waiting;
        // A stopped one waits for an explicit preparation alone.
        if waits == PreparationWait::Stopped {
            return Ok(());
        }
        match self.preparation_gate(&workspace, for_agent).await? {
            Gate::Closed(now) if now == waits => Ok(()),
            Gate::Closed(now) => (self.note_preparation_wait(&workspace, (&waits, &now))).await,
            Gate::Open { base } => {
                self.release_preparation(&workspace, base).await;
                Ok(())
            }
        }
    }

    /// Start the waiting preparation of the workspace `id`, creating its
    /// worktree on `base` when given. A preparation that cannot start fails
    /// as one that ran would, which its start's coordinator or the user
    /// hears of.
    async fn release_preparation(self: &Arc<Self>, id: &SpaceId, base: Option<String>) {
        let released = async {
            let Some(Claimed { steps, env, scope }) = self.claim_release(id, base).await? else {
                return Ok(None);
            };
            let caller = scope.map(Caller::of_space);
            let workspace = SpaceSelector::id(id);
            let spec = OperationSpec::Prepare { workspace, steps };
            self.start_operation(spec, env, caller.as_ref())
                .await
                .map(Some)
        };
        match released.await {
            Ok(Some(operation)) => self.report_preparation_failure(id, &operation),
            Ok(None) => {}
            Err(error) => {
                let message = format!("{error:#}");
                (self.tell_preparation_failure(id, &Failure::Failed { message })).await;
            }
        }
    }

    /// Take the waiting preparation of the workspace `id` out of its record
    /// to start it, creating its worktree on `base` when given, with the
    /// workspace that owns it as its base, in one transaction under the
    /// gates of a base change. A base it cannot take, such as one that
    /// builds on it, ends the wait all the same, so the release fails once.
    async fn claim_release(&self, id: &SpaceId, base: Option<String>) -> Result<Option<Claimed>> {
        let base = base.map(BranchName::from);
        let owner = match &base {
            Some(base) => self.branch_owner(id, base).await?,
            None => None,
        };
        let _gate = self.gates.lock_prs(owner.iter().chain([id])).await;
        let claiming = id.clone();
        let claimed = (self.store)
            .write(move |tx| {
                let claimed = claim_waiting(tx, &claiming)?;
                if let (Some(_), Some(base)) = (&claimed, &base) {
                    build_pending_in(tx, &claiming, base, owner.as_ref())?;
                }
                Ok(claimed)
            })
            .await;
        if claimed.is_err() {
            let ending = id.clone();
            self.store.run(move |db| end_wait(db, &ending)).await?;
        }
        claimed
    }

    /// Record the preparation `start` of the workspace `id`, in the scope
    /// of the space `scope`, waiting for what `waits` says.
    async fn record_waiting_preparation(
        &self,
        id: &SpaceId,
        start: &PreparationStart,
        scope: Option<SpaceId>,
        waits: &PreparationWait,
    ) -> Result<()> {
        let (id, for_agent) = (id.clone(), start.for_agent);
        let steps = serde_json::to_string(&start.steps)?;
        let env = (start.env.as_ref())
            .map(serde_json::to_string)
            .transpose()?;
        let waits = serde_json::to_string(waits)?;
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT INTO waiting_preparations(workspace_id,steps,env,scope_space_id,for_agent,waits)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![id, steps, env, scope, for_agent, waits],
                )?;
                Ok(())
            })
            .await
    }

    /// Record that the waiting preparation of the workspace `id`, which
    /// waited for `was`, now waits for `now`, unless what it waits for
    /// changed since, as a stop that cancelled its agent changes it.
    async fn note_preparation_wait(
        &self,
        id: &SpaceId,
        (was, now): (&PreparationWait, &PreparationWait),
    ) -> Result<()> {
        let id = id.clone();
        let (was, now) = (serde_json::to_string(was)?, serde_json::to_string(now)?);
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE waiting_preparations SET waits=?3 WHERE workspace_id=?1 AND waits=?2",
                    params![id, was, now],
                )?;
                Ok(())
            })
            .await
    }
}

/// What the waiting preparation of the workspace `id` waits for, if one
/// does.
pub(crate) fn preparation_waits(
    db: &rusqlite::Connection,
    id: &SpaceId,
) -> Result<Option<PreparationWait>> {
    let waits: Option<String> = db
        .query_row(
            "SELECT waits FROM waiting_preparations WHERE workspace_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(waits
        .map(|waits| serde_json::from_str(&waits))
        .transpose()?)
}

/// Record that the waiting preparation of the workspace `id`, if one
/// waits, is stopped, as a stop or cancelling its agent's launch leaves
/// it, until an explicit preparation runs it.
pub(crate) fn note_stopped(db: &rusqlite::Connection, id: &SpaceId) -> Result<()> {
    db.execute(
        "UPDATE waiting_preparations SET waits=?2 WHERE workspace_id=?1",
        params![id, stopped()?],
    )?;
    Ok(())
}

/// [`PreparationWait::Stopped`] as its record keeps it.
fn stopped() -> Result<String> {
    Ok(serde_json::to_string(&PreparationWait::Stopped)?)
}

/// Every waiting preparation.
fn waiting_preparations(db: &mut rusqlite::Connection) -> Result<Vec<Waiting>> {
    let rows = db
        .prepare("SELECT workspace_id,for_agent,waits FROM waiting_preparations")?
        .query_map([], |row| {
            Ok((
                row.get::<_, SpaceId>(0)?,
                row.get(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(workspace, for_agent, waits)| {
            Ok(Waiting {
                workspace,
                for_agent,
                waits: serde_json::from_str(&waits)?,
            })
        })
        .collect()
}

/// End the wait of the preparation of the workspace `id`, for one that
/// starts another way.
fn end_wait(db: &mut rusqlite::Connection, id: &SpaceId) -> Result<()> {
    db.execute(
        "DELETE FROM waiting_preparations WHERE workspace_id=?1",
        [id],
    )?;
    Ok(())
}

/// Take the waiting preparation of the workspace `id` out of its record to
/// start it, if it still waits, unstopped, and, where it waits for an
/// agent, the launch of that agent still waits to open the workspace: a
/// stop since its gate was read keeps it waiting.
fn claim_waiting(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<Claimed>> {
    let claimed: Option<(String, Option<String>, Option<SpaceId>)> = db
        .query_row(
            "DELETE FROM waiting_preparations WHERE workspace_id=?1 AND waits IS NOT ?4
                AND (NOT for_agent OR EXISTS (SELECT 1 FROM agent_queue
                    WHERE opens_workspace_id=?1 AND kind=?2 AND state=?3))
             RETURNING steps,env,scope_space_id",
            params![id, QueueKind::Launch, QueueState::Queued, stopped()?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((steps, env, scope)) = claimed else {
        return Ok(None);
    };
    Ok(Some(Claimed {
        steps: serde_json::from_str(&steps)?,
        env: env.map(|env| serde_json::from_str(&env)).transpose()?,
        scope,
    }))
}

#[cfg(test)]
mod tests;
