//! The end of a claimed job: once the execution running the forge's runner
//! ends, however it ended, the job's workspace is removed whatever it holds,
//! with its permit, and so is the runner's directory of the job.
use std::time::Duration;

use anyhow::Result;
use rusqlite::params;

use crate::{
    daemon::{
        notifications::NotificationKind,
        workspace::{
            Manager,
            capacity::{self, Activity},
        },
    },
    model::QueuedJob,
    state::{QueueKind, QueueState},
};

/// A job the runner claimed.
#[derive(Debug, Clone)]
pub(crate) struct ClaimedJob {
    /// The ID its queue entry had.
    pub id: String,
    pub workspace_id: String,
    /// The execution running the forge's runner, once it started.
    pub execution_id: Option<String>,
    pub job: QueuedJob,
}

impl Manager {
    /// The jobs the runner claimed.
    pub(crate) async fn claimed_jobs(&self) -> Result<Vec<ClaimedJob>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT id,workspace_id,execution_id,job FROM ci_jobs ORDER BY started_at",
                    )?
                    .query_map([], |row| {
                        Ok(ClaimedJob {
                            id: row.get(0)?,
                            workspace_id: row.get(1)?,
                            execution_id: row.get(2)?,
                            job: serde_json::from_str(&row.get::<_, String>(3)?).map_err(
                                |error| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        3,
                                        rusqlite::types::Type::Text,
                                        Box::new(error),
                                    )
                                },
                            )?,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Remove the workspace of each claimed job whose execution ended, or
    /// that never started one and whose claim no longer runs, as after a
    /// daemon restart. A removal that fails is tried again on the next pass.
    pub(crate) async fn finish_ended_jobs(&self) -> Result<()> {
        for job in self.claimed_jobs().await? {
            let running = match &job.execution_id {
                Some(execution) => self.execution_recorded(execution).await?,
                None => self.runners.claiming(&job.id),
            };
            if running {
                continue;
            }
            if let Err(error) = self.remove_job_workspace(&job.workspace_id).await {
                self.notify(
                    None,
                    NotificationKind::RunnerFailed,
                    format!(
                        "The workspace of the ended CI job {} could not be removed: {error:#}",
                        job.job.job.name
                    ),
                )
                .await;
                continue;
            }
            match std::fs::remove_dir_all(self.job_directory(&job.id)) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    crate::daemon::log!("runner: {error:#}");
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Whether the daemon still records the execution `id`, running or
    /// awaiting reattachment.
    async fn execution_recorded(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(db, "SELECT 1 FROM executions WHERE id=?1", [id])
            })
            .await
    }

    /// The load of each admitted or claimed job, by how long ago it was
    /// admitted.
    pub(crate) async fn job_activities(&self) -> Result<Vec<Activity>> {
        let since: Vec<Option<i64>> = self
            .store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT seen_at FROM agent_queue WHERE kind=?1 AND state=?2
                         UNION ALL SELECT started_at FROM ci_jobs",
                    )?
                    .query_map(params![QueueKind::Job, QueueState::Starting], |row| {
                        row.get(0)
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await?;
        let now = crate::time::unix_seconds() as i64;
        Ok(since
            .into_iter()
            .map(|since| Activity::Job {
                age: Duration::from_secs(now.saturating_sub(since.unwrap_or(now)).max(0) as u64),
                cores: capacity::ASSUMED_AGENT_CORES,
            })
            .collect())
    }
}
