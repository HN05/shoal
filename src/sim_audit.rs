use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    model::Workspace,
    simulators::{SimRequest, now},
    workspace::Manager,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct EvictedDevice {
    pub id: String,
    pub udid: Option<String>,
    pub installed_apps: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CleanRequest {
    pub workspace_id: String,
    pub workspace_name: String,
    pub repository_id: String,
    pub execution_id: Option<String>,
    pub requested_at: u64,
    pub updated_at: u64,
    pub attempts: u64,
    pub request: SimRequest,
    pub status: String,
    pub action: Option<String>,
    pub simulator_id: Option<String>,
    pub udid: Option<String>,
    pub apps_removed: Option<usize>,
    pub erase_completed: bool,
    pub evicted: Vec<EvictedDevice>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    #[serde(flatten)]
    pub request: CleanRequest,
}

impl Manager {
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
                audit.status == "busy",
                "clean request already completed or interrupted; inspect its history before retrying with a new request ID"
            );
            audit.status = "requested".into();
            audit.updated_at = now();
            audit.attempts += 1;
            audit.error = None;
            audit
        } else {
            CleanRequest {
                workspace_id: workspace.id.clone(),
                workspace_name: workspace.name.clone(),
                repository_id: workspace.repository_id.clone(),
                execution_id,
                requested_at: now(),
                updated_at: now(),
                attempts: 1,
                request: request.clone(),
                status: "requested".into(),
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

    pub async fn save_clean_request(&self, audit: &CleanRequest) -> Result<()> {
        let (id, owner, record) = (
            audit.request.request_id.clone(),
            audit.workspace_id.clone(),
            serde_json::to_string(audit)?,
        );
        self.store.run(move |db| {
            db.execute("INSERT INTO simulator_clean_requests(request_id,workspace_id,record) VALUES (?1,?2,?3) ON CONFLICT(request_id) DO UPDATE SET record=excluded.record", params![id,owner,record])?;
            Ok(())
        }).await
    }

    pub async fn clean_history(
        &self,
        owner: Option<String>,
        limit: u32,
        before: Option<i64>,
    ) -> Result<Vec<AuditEntry>> {
        ensure!(
            (1..=50).contains(&limit),
            "history limit must be between 1 and 50"
        );
        self.store.run(move |db| {
            let records = db.prepare("SELECT id,record FROM simulator_clean_requests WHERE (?1 IS NULL OR workspace_id=?1) AND (?2 IS NULL OR id<?2) ORDER BY id DESC LIMIT ?3")?
                .query_map(params![owner,before,limit], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            records.into_iter().map(|(id, record)| Ok(AuditEntry { id, request: serde_json::from_str(&record)? })).collect()
        }).await
    }
}
