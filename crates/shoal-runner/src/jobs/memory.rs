//! What the scope a job's runner ran in recorded of its memory, which the
//! wrapper reports as the runner ends: the most it held raises the job's
//! peak memory, which its cost reads, and processes of it the kernel's OOM
//! killer killed, such as a build's compiler, are named in the job's end
//! in its workspace's log.
use anyhow::Result;
use shoal_daemon_base::events::record_job_ended;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::protocol::ScopeMemory;

use super::overrun::ended_job;
use crate::{Runner, log};

impl Runner {
    /// Keep what the scope of the job the ended execution `id` ran recorded
    /// of its `memory`; nothing for an execution that ran no job.
    pub async fn record_job_memory(&self, id: &ExecutionId, memory: ScopeMemory) -> Result<()> {
        let jobs = self.claimed_jobs().await?;
        let Some(job) = (jobs.into_iter()).find(|job| job.execution_id.as_ref() == Some(id)) else {
            return Ok(());
        };
        self.raise_job_peak(&job.id, memory.peak).await?;
        if memory.oom_kills == 0 {
            return Ok(());
        }
        let name = job.job.job.name.clone();
        let reason = format!(
            "the kernel's OOM killer killed {} of its processes for memory",
            memory.oom_kills
        );
        log!("runner: {name}: {reason}");
        let ended = ended_job(name, job.job.run.url.to_string(), reason, Vec::new());
        record_job_ended(&self.store, &job.workspace_id, ended).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
