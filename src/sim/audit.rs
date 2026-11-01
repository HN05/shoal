//! Audit trail for clean-device requests. Persisted before any destructive
//! simctl action and retained after the workspace is gone.
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};

pub(crate) use crate::client::sim::{
    AuditEntry, CleanAction, CleanRequest, CleanRequestStatus, EvictedDevice,
};
use crate::{
    daemon::{allocation::Allocation, approvals::DecisionStatus, workspace::Manager},
    model::Workspace,
    sim::{SimRequest, Simulator},
    time::unix_seconds,
};

impl CleanRequest {
    /// Record an idle device about to be deleted to make room.
    pub(super) fn record_eviction(&mut self, sim: &Simulator) {
        self.evicted.push(EvictedDevice {
            id: sim.id.clone(),
            udid: sim.udid.clone(),
            installed_apps: sim.installed_apps,
        });
        self.action = Some(CleanAction::CreateAfterEviction);
    }

    /// Record the device the request will use and how it becomes clean.
    pub(super) fn record_selection(&mut self, sim: &Simulator, needs_reset: bool) {
        self.simulator_id = Some(sim.id.clone());
        self.udid = sim.udid.clone();
        self.apps_removed = needs_reset.then_some(sim.installed_apps).flatten();
        if self.action.is_none() {
            self.action = Some(if needs_reset {
                CleanAction::Erase
            } else {
                CleanAction::Create
            });
        }
    }

    /// Record how the allocation ended. Only a busy outcome, including a
    /// pending approval, lets the same request ID retry.
    fn record_outcome(&mut self, result: &Result<Allocation<Box<Simulator>>>) {
        self.updated_at = unix_seconds();
        match result {
            Ok(Allocation::Granted(sim)) => {
                self.status = CleanRequestStatus::Acquired;
                self.simulator_id = Some(sim.id.clone());
                self.udid = sim.udid.clone();
            }
            Ok(Allocation::Busy(message)) => {
                self.status = CleanRequestStatus::Busy;
                self.error = Some(message.clone());
            }
            Ok(Allocation::Approval(request)) => {
                self.status = if request.status == DecisionStatus::Denied {
                    CleanRequestStatus::Failed
                } else {
                    CleanRequestStatus::Busy
                };
                self.error = Some(format!("approval {} ({})", request.status, request.id));
            }
            Err(error) => {
                self.status = CleanRequestStatus::Failed;
                self.error = Some(format!("{error:#}"));
            }
        }
    }
}

impl Manager {
    /// Record a clean request before acting on it. A busy request may be
    /// retried under the same ID; anything else needs a new one.
    pub async fn start_clean_request(
        &self,
        workspace: &Workspace,
        request: &SimRequest,
        execution_id: Option<String>,
    ) -> Result<CleanRequest> {
        uuid::Uuid::parse_str(&request.request_id)?;
        let id = request.request_id.clone();
        let previous = self
            .store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT record FROM simulator_clean_requests WHERE request_id=?1",
                        [id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?)
            })
            .await?;
        let audit = if let Some(previous) = previous {
            let mut audit: CleanRequest = serde_json::from_str(&previous)?;
            ensure!(
                audit.workspace_id == workspace.id
                    && audit.execution_id == execution_id
                    && &audit.request == request,
                "clean request ID already belongs to a different request"
            );
            ensure!(
                audit.status == CleanRequestStatus::Busy,
                "clean request already completed or interrupted; inspect its history before retrying with a new request ID"
            );
            audit.status = CleanRequestStatus::Requested;
            audit.updated_at = unix_seconds();
            audit.attempts += 1;
            audit.error = None;
            audit
        } else {
            CleanRequest {
                workspace_id: workspace.id.clone(),
                workspace_name: workspace.name.clone(),
                repository_id: workspace.repository_id.clone(),
                execution_id,
                requested_at: unix_seconds(),
                updated_at: unix_seconds(),
                attempts: 1,
                request: request.clone(),
                status: CleanRequestStatus::Requested,
                action: None,
                simulator_id: None,
                udid: None,
                apps_removed: None,
                erase_completed: false,
                evicted: vec![],
                error: None,
            }
        };
        // Audit persistence is required before any destructive action. No FK to
        // workspaces: deleting a worktree must not delete its accountability trail.
        self.save_clean_request(&audit).await?;
        Ok(audit)
    }

    /// Persist the allocation outcome of a clean request started by
    /// `start_clean_request`.
    pub(super) async fn finish_clean_request(
        &self,
        mut audit: CleanRequest,
        result: &Result<Allocation<Box<Simulator>>>,
    ) -> Result<()> {
        audit.record_outcome(result);
        self.save_clean_request(&audit).await
    }

    pub async fn save_clean_request(&self, audit: &CleanRequest) -> Result<()> {
        let (id, owner, record) = (
            audit.request.request_id.clone(),
            audit.workspace_id.clone(),
            serde_json::to_string(audit)?,
        );
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT INTO simulator_clean_requests(request_id,workspace_id,record) VALUES (?1,?2,?3) ON CONFLICT(request_id) DO UPDATE SET record=excluded.record",
                    params![id, owner, record],
                )?;
                Ok(())
            })
            .await
    }

    /// The clean requests of the workspace `selector` names, or of all of
    /// them, newest first.
    pub async fn clean_history(
        &self,
        selector: Option<&str>,
        limit: u32,
        before: Option<i64>,
    ) -> Result<Vec<AuditEntry>> {
        let owner = self.workspace_filter(selector).await?;
        ensure!(
            (1..=50).contains(&limit),
            "history limit must be between 1 and 50"
        );
        self.store
            .run(move |db| history(db, owner.as_deref(), limit, before))
            .await
    }
}

/// Up to `limit` clean requests before `before`, of `owner` or of every
/// workspace, newest first.
fn history(
    db: &rusqlite::Connection,
    owner: Option<&str>,
    limit: u32,
    before: Option<i64>,
) -> Result<Vec<AuditEntry>> {
    let records = db
        .prepare(
            "SELECT id,record FROM simulator_clean_requests WHERE (?1 IS NULL OR workspace_id=?1) AND (?2 IS NULL OR id<?2) ORDER BY id DESC LIMIT ?3",
        )?
        .query_map(params![owner, before, limit], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    records
        .into_iter()
        .map(|(id, record)| {
            Ok(AuditEntry {
                id,
                request: serde_json::from_str(&record)?,
            })
        })
        .collect()
}
