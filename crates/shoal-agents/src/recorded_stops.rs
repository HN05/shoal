//! What the daemon keeps of a stopped agent before its execution's record
//! goes: the turn it last reported, and the stop whose cause its wrapper saved
//! in its recovery record, which the daemon logs whether or not the wrapper's
//! pause report reached it.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::stop_causes::StopCause;

use crate::Agents;
use crate::execution::recovery::{self, RecordExt as _};

impl Agents {
    /// Keep what the stopped agent of execution `id` left, as every path that
    /// forgets an execution does first: its turn, and the stop its wrapper
    /// recorded.
    pub async fn keep_stopped_agent(&self, id: &ExecutionId) {
        self.keep_stopped_turn(id, None).await;
        self.settle_recorded_stop(id).await;
    }

    /// Log the stop whose cause the recovery record of execution `id` names,
    /// and count an OOM kill, unless the wrapper's pause report did. Runs
    /// while the execution is still recorded. A record that cannot be read
    /// leaves the stop unlogged.
    async fn settle_recorded_stop(&self, id: &ExecutionId) {
        let query = id.clone();
        let found = (self.store)
            .run(move |db| shoal_store::find_execution(db, &query))
            .await;
        let Ok(Some(execution)) = found else {
            return;
        };
        let space_id = &execution.space_id;
        let path = recovery::record_path(&self.paths, space_id, id);
        let Some(record) = recovery::Record::load(&path).ok().flatten() else {
            return;
        };
        let Some(cause) = record.cause else {
            return;
        };
        self.log_recorded_stop(space_id, id, &record, cause).await;
        if cause == StopCause::OomKill {
            self.record_oom_kill(space_id, id).await;
        }
    }

    /// The cause the recovery record of `execution` in the space `id` names,
    /// as a wrapper that reattaches while it waits finds it after a daemon
    /// restart cut off its pause report.
    pub fn recorded_cause(&self, id: &SpaceId, execution: &ExecutionId) -> Option<StopCause> {
        let path = recovery::record_path(&self.paths, id, execution);
        recovery::Record::load(&path).ok().flatten()?.cause
    }

    /// Log that the agent of `record`, which `execution` ran in the space
    /// `id`, stopped for `cause`, unless its pause report logged it already.
    async fn log_recorded_stop(
        &self,
        id: &SpaceId,
        execution: &ExecutionId,
        record: &recovery::Record,
        cause: StopCause,
    ) {
        let stopped = shoal_daemon_base::events::AgentEvent {
            name: record.agent.clone(),
            execution_id: Some(execution.clone()),
            mode: Some(record.mode.clone()),
            exit_code: None,
            oom_kills: 0,
            cause: Some(cause),
            source: None,
        };
        let kind = shoal_daemon_base::events::EventKind::AgentStopped;
        shoal_daemon_base::events::record_agent(&self.store, id, kind, stopped).await;
    }
}

#[cfg(test)]
mod tests;
