//! Caller-named claims of a space's use, a workspace's or a chat's,
//! independent of assignment completion.
use crate::daemon::approvals::ApprovalRequestExt;
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;

use super::{GuardMode, Manager, ResourceGuard};
use crate::{
    config::permissions::PermissionKey,
    daemon::{
        allocation::Allocation,
        approvals::{
            self, ApprovalRequest, HoldSpecification, Lifetime, ReasonRequired, Specification,
            Target,
        },
        permissions::Action,
        scope::Caller,
        store,
    },
    model::{HoldPlacer, PresentSpace, SpaceHold},
    validate,
};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Hold the space as `placed_by`; repeating a held name keeps the hold
    /// as it was placed.
    pub async fn acquire_hold(
        &self,
        selector: &SpaceSelector,
        name: String,
        reason: Option<String>,
        placed_by: HoldPlacer,
    ) -> Result<SpaceHold> {
        self.place_hold(selector, name, reason, placed_by, Authority::Caller)
            .await
    }

    /// Hold the space as `placed_by` once `authority` allows it in the
    /// transaction that places the hold.
    async fn place_hold(
        &self,
        selector: &SpaceSelector,
        name: String,
        reason: Option<String>,
        placed_by: HoldPlacer,
        authority: Authority,
    ) -> Result<SpaceHold> {
        validate::name("hold", &name)?;
        validate::reason("hold", reason.as_deref())?;
        let space = self.space(selector).await?;
        let _guard = self.hold_guard(&space).await?;
        let id = space.id().clone();
        let removing = self.space_removals.clone();
        let hold = (self.store)
            .write(move |tx| {
                ensure_holdable(tx, &id, &removing)?;
                place(tx, &id, &name, reason.as_deref(), placed_by, authority)
            })
            .await?;
        self.touch(space.id()).await;
        Ok(hold)
    }

    /// What placing a hold on `space` holds meanwhile: no resource hook runs
    /// in the worktree of a workspace, which must still be its own.
    async fn hold_guard(&self, space: &PresentSpace) -> Result<ResourceGuard> {
        let guard = (self.gates).resource_guard(space.id(), GuardMode::Shared)?;
        if let Some(workspace) = space.worktree() {
            self.verify_worktree(workspace).await?;
        }
        Ok(guard)
    }

    /// Hold the space for its own agent once its coordinator, or the user
    /// when it has none, approves; approval places the hold, and asking
    /// again returns it.
    pub async fn request_hold(
        &self,
        selector: &SpaceSelector,
        name: String,
        reason: Option<String>,
    ) -> Result<Allocation<SpaceHold>> {
        validate::name("hold", &name)?;
        let space = self.space(selector).await?;
        let id = space.id();
        // Acquiring returns the held one, with every check acquisition makes.
        if space.holds().iter().any(|hold| hold.name == name) {
            return Ok(Allocation::Granted(
                self.place_hold(
                    &SpaceSelector::id(id),
                    name,
                    reason,
                    HoldPlacer::Agent,
                    Authority::Approval,
                )
                .await?,
            ));
        }
        let request = ApprovalRequest::new(
            id,
            Target::Hold(name.clone()),
            &name,
            Specification::Hold(HoldSpecification { hold: name.clone() }),
            Lifetime::Lease,
            reason.as_deref(),
        );
        let pending = self.request_approval(request).await.map_err(|error| {
            if error.is::<ReasonRequired>() {
                anyhow!(
                    "holding your own space needs approval; request it with shoal acquire hold --name {name} --reason <why>"
                )
            } else {
                error
            }
        })?;
        let outcome = match pending {
            Some(pending) => Allocation::Approval(Box::new(pending)),
            None => Allocation::Granted(
                self.place_hold(
                    &SpaceSelector::id(id),
                    name,
                    reason,
                    HoldPlacer::Agent,
                    Authority::Approval,
                )
                .await?,
            ),
        };
        (self.notify_allocation((space.name(), id), &outcome, "")).await;
        Ok(outcome)
    }

    /// What approving `request` needs held while its decision places the
    /// hold it asks for, as acquiring one directly does. `None` for other
    /// requests and denials.
    pub(crate) async fn hold_decision_guard(
        &self,
        request: &ApprovalRequest,
        approve: bool,
    ) -> Result<Option<ResourceGuard>> {
        if !approve || !matches!(request.target, Target::Hold(_)) {
            return Ok(None);
        }
        let space = self.space_by_id(&request.space_id).await?;
        Ok(Some(self.hold_guard(&space).await?))
    }

    /// Release the hold `name` for `released_by`: an agent releases only
    /// the holds agents placed.
    pub async fn release_hold(
        &self,
        selector: &SpaceSelector,
        name: String,
        released_by: HoldPlacer,
    ) -> Result<()> {
        validate::name("hold", &name)?;
        let space = self.space(selector).await?;
        let id = space.id().clone();
        self.store
            .write(move |tx| release(tx, &id, &name, released_by))
            .await?;
        self.touch(space.id()).await;
        self.signals.wake_cleanup();
        Ok(())
    }

    /// Release the hold `name` for `caller`: a scoped caller releases one
    /// the user placed as its `user_holds` permission allows.
    pub(crate) async fn release_hold_as(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
        name: String,
    ) -> Result<()> {
        let Some(caller) = caller else {
            return self.release_hold(selector, name, HoldPlacer::User).await;
        };
        let space = self.space(selector).await?;
        let users = (space.holds().iter())
            .any(|hold| hold.name == name && hold.placed_by == HoldPlacer::User);
        if !users {
            return self.release_hold(selector, name, HoldPlacer::Agent).await;
        }
        // A name may repeat, so the space's ID binds the approval.
        let release = format!(
            "release the user's hold {name} on {} ({})",
            space.name(),
            space.id()
        );
        let action = Action {
            repository: space.repository_id().cloned(),
            ..Action::new(PermissionKey::UserHolds, release)
        };
        self.authorize_action(Some(caller), action).await?;
        self.release_hold(selector, name, HoldPlacer::User).await
    }

    pub async fn holds(&self, selector: &SpaceSelector) -> Result<Vec<SpaceHold>> {
        Ok(self.space(selector).await?.holds().to_vec())
    }

    /// Whether the user placed a hold on the space `id`.
    pub(crate) async fn user_holds(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        (self.store)
            .run(move |db| {
                store::exists(
                    db,
                    "SELECT 1 FROM space_holds WHERE space_id=?1 AND placed_by=?2",
                    params![id, HoldPlacer::User],
                )
            })
            .await
    }

    pub(crate) async fn has_holds(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| store::exists(db, "SELECT 1 FROM space_holds WHERE space_id=?1", [id]))
            .await
    }
}

/// What lets a hold be placed.
#[derive(Clone, Copy)]
enum Authority {
    /// The caller may hold the space, as the user or an agent above it.
    Caller,
    /// The space's own agent holds it only with its approval: the hold
    /// already placed, or an approved request, either of which a release
    /// meanwhile ends.
    Approval,
}

impl Authority {
    fn ensure(self, tx: &rusqlite::Transaction<'_>, id: &SpaceId, name: &str) -> Result<()> {
        if let Authority::Caller = self {
            return Ok(());
        }
        let held = store::exists(
            tx,
            "SELECT 1 FROM space_holds WHERE space_id=?1 AND name=?2",
            params![id, name],
        )?;
        let approved = approvals::current(tx, id, &Target::Hold(name.to_owned()), name)?
            .is_some_and(|request| request.status == approvals::DecisionStatus::Approved);
        ensure!(
            held || approved,
            "the approval for hold {name} ended; request it again with shoal acquire hold --name {name} --reason <why>"
        );
        Ok(())
    }
}

/// Refuse a hold on a space a removal has reserved, which has read its holds
/// already, or on a workspace that is not ready. A reservation precedes the
/// removal's read on the store's one thread, so that read sees every hold
/// this lets through.
fn ensure_holdable(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    removing: &crate::daemon::spaces::Removing,
) -> Result<()> {
    ensure!(!removing.reserved(id), "the space is being removed");
    crate::daemon::links::space::require_ready_space(tx, id)
}

/// Place the hold once `authority` allows it and return it as it stands: a
/// held name keeps the hold as it was placed.
fn place(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    name: &str,
    reason: Option<&str>,
    placed_by: HoldPlacer,
    authority: Authority,
) -> Result<SpaceHold> {
    authority.ensure(tx, id, name)?;
    insert(tx, id, name, reason, placed_by)?;
    let hold = tx.query_row(
        &format!("{COLUMNS} WHERE space_id=?1 AND name=?2"),
        params![id, name],
        row,
    )?;
    Ok(hold)
}

/// Delete the hold for `released_by`, who as an agent releases only the
/// holds agents placed, and end its approval, so holding it again needs
/// another.
fn release(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    name: &str,
    released_by: HoldPlacer,
) -> Result<()> {
    let placed_by: Option<HoldPlacer> = tx
        .query_row(
            "SELECT placed_by FROM space_holds WHERE space_id=?1 AND name=?2",
            params![id, name],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        released_by == HoldPlacer::User || placed_by != Some(HoldPlacer::User),
        "hold {name} is the user's; scoped processes release only the holds agents placed"
    );
    tx.execute(
        "DELETE FROM space_holds WHERE space_id=?1 AND name=?2",
        params![id, name],
    )?;
    approvals::release(tx, id, &Target::Hold(name.to_owned()), name)?;
    Ok(())
}

/// Place, in the transaction that approved it, the hold `request` asks for,
/// refusing it where [`ensure_holdable`] does: a decision never stands
/// without its hold.
pub(crate) fn place_approved(
    tx: &rusqlite::Transaction<'_>,
    request: &ApprovalRequest,
    removing: &crate::daemon::spaces::Removing,
) -> Result<()> {
    let Target::Hold(name) = &request.target else {
        return Ok(());
    };
    ensure_holdable(tx, &request.space_id, removing)?;
    let reason = Some(request.reason.as_str()).filter(|reason| !reason.is_empty());
    insert(tx, &request.space_id, name, reason, HoldPlacer::Agent)
}

/// Insert the hold unless the space already has one by that name.
fn insert(
    db: &Connection,
    id: &SpaceId,
    name: &str,
    reason: Option<&str>,
    placed_by: HoldPlacer,
) -> Result<()> {
    db.execute(
        "INSERT INTO space_holds(space_id,name,reason,created_at,placed_by) VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(space_id,name) DO NOTHING",
        params![id, name, reason, crate::time::UnixTime::now(), placed_by],
    )?;
    Ok(())
}

const COLUMNS: &str = "SELECT space_id,name,reason,created_at,placed_by FROM space_holds";

/// The holds on the space `id`, by name.
pub(crate) fn list(db: &Connection, id: &SpaceId) -> Result<Vec<SpaceHold>> {
    Ok(db
        .prepare(&format!("{COLUMNS} WHERE space_id=?1 ORDER BY name"))?
        .query_map([id], row)?
        .collect::<rusqlite::Result<_>>()?)
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SpaceHold> {
    Ok(SpaceHold {
        space_id: row.get(0)?,
        name: row.get(1)?,
        reason: row.get(2)?,
        created_at: row.get(3)?,
        placed_by: row.get(4)?,
    })
}

#[cfg(test)]
mod tests;
