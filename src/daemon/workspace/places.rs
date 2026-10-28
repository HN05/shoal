//! Where executions run in terminal hosts, as their wrappers report it.
use anyhow::Result;
use rusqlite::params;

use super::Manager;
use crate::sessions::Origin;

impl Manager {
    /// Record that `execution` runs at `origin`.
    pub(crate) async fn record_execution_origin(
        &self,
        execution: &str,
        origin: Origin,
    ) -> Result<()> {
        let execution = execution.to_owned();
        let origin = serde_json::to_string(&origin)?;
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE executions SET origin=?2 WHERE id=?1",
                    params![execution, origin],
                )?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
