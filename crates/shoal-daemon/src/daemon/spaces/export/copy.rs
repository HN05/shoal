//! Copies of spaces that keep running: a space with its agents and the
//! sessions they reported, a workspace at a checkpoint, with copies of the
//! spaces below it that the package carries, which `shoal restore --from`
//! opens as new spaces whose agents start from forks.
use crate::model::EventCause;
use anyhow::Result;

use super::Prepared;
use crate::{
    daemon::manager::Manager,
    execution::recovery::{self, Copied, Record, StoppedAgent},
    model::{Execution, RemovedSpace, Space},
    time::UnixTime,
    transfer::Carrying,
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Write a copy of the space `selector` names, with copies of the
    /// spaces below it that `carrying` carries, for another Shoal to the
    /// new file `file`, leaving them running here: a workspace at its
    /// checkpoint `checkpoint`, or at a new one, and those below at new
    /// ones.
    pub async fn copy_space(
        &self,
        selector: &SpaceSelector,
        checkpoint: Option<CheckpointNumber>,
        file: std::path::PathBuf,
        carrying: &Carrying,
    ) -> Result<crate::transfer::ExportedSpace> {
        crate::daemon::access::ensure_writable(crate::transfer::directory(&file)).await?;
        let space = self.running_space(selector).await?;
        let copied = self.prepare_copy(space, checkpoint, carrying).await?;
        self.write_prepared(copied, file).await
    }

    /// The workspace or running chat `selector` names, refusing a removed
    /// space, which runs nothing to keep.
    pub(super) async fn running_space(&self, selector: &SpaceSelector) -> Result<Space> {
        let error = match self.space(selector).await {
            Ok(space) => return Ok(space),
            Err(error) => error,
        };
        let removed = self
            .removed_spaces()
            .await?
            .into_iter()
            .find(|removed| selector.given_id() == Some(&removed.space_id));
        match removed {
            Some(removed) => anyhow::bail!(
                "{} was removed, so nothing runs to keep; export it without --keep, or shoal restore {} opens it again",
                removed.name,
                removed.space_id
            ),
            None => Err(error),
        }
    }

    /// A copy of `space` with copies of the spaces below it that
    /// `carrying` carries, as [`Self::copy_record_of`] records each: the
    /// space at its checkpoint `number`, or at a new one, and those below
    /// at new ones. An earlier checkpoint marks the sessions as newer than
    /// its files.
    async fn prepare_copy(
        &self,
        space: Space,
        number: Option<CheckpointNumber>,
        carrying: &Carrying,
    ) -> Result<Prepared> {
        let carried = self.carried_by(&space.id, carrying).await?;
        let copied = self.copy_record_of(&space, number).await?;
        let mut records = Vec::new();
        for below in &carried {
            records.push(self.copy_record_of(below, None).await?);
        }
        // Each one's own config may describe its agents, and goes with it.
        let settings = self
            .agent_settings_of(std::iter::once(&space).chain(&carried))
            .await;
        let mut prepared = Prepared::of(self, copied, records, settings).await?;
        prepared.manifest.copy = Some(Copied {
            earlier_files: number.is_some(),
            held_elsewhere: false,
        });
        Ok(prepared)
    }

    /// A copy of `space`, as its removal would record it now, with its
    /// agents, which start from forks of the sessions they reported: a
    /// workspace at its checkpoint `number`, or at a new one.
    async fn copy_record_of(
        &self,
        space: &Space,
        number: Option<CheckpointNumber>,
    ) -> Result<RemovedSpace> {
        let checkpoint = self.copy_checkpoint(space, number).await?;
        let mut copied = self
            .present_removal_record(&space.id, checkpoint, EventCause::Manual)
            .await?;
        copied.agents = self.copied_agents(&space.id).await?;
        // Its sessions' files are the original's, which the package carries
        // from where they are.
        copied.session_files = Vec::new();
        Ok(copied)
    }

    /// The agents of the space `id` a copy starts again, each with the
    /// session it reported last: those stopped, then those its executions
    /// run.
    async fn copied_agents(&self, id: &SpaceId) -> Result<Vec<StoppedAgent>> {
        let mut agents = recovery::stopped_agents(&self.paths, id)?;
        let now = UnixTime::now();
        for execution in self.workspace_executions(id).await? {
            if agents.iter().any(|agent| agent.execution == execution.id) {
                continue;
            }
            let session = recovery::reported_session(&self.paths, id, &execution.id);
            let Some(record) = running_record(&execution, session) else {
                continue;
            };
            agents.push(StoppedAgent {
                execution: execution.id,
                record,
                stopped_at: now,
            });
        }
        Ok(agents)
    }
}

/// The record a copy keeps of the agent `execution` runs, with the `session`
/// it reported; `None` for an execution without an agent.
fn running_record(
    execution: &Execution,
    session: Option<shoal_ids::agent_session_ids::AgentSessionId>,
) -> Option<Record> {
    Some(Record {
        narrowing: execution.narrowing,
        // Named as recovery records name it.
        agent: execution.agent.as_ref()?.normalized(),
        stop_reason: None,
        stage: None,
        origin: None,
        waiting: None,
        session,
        moved: false,
        copied: None,
        converted: false,
        turn: None,
        for_load: false,
        model: execution.model.clone(),
        mode: execution
            .mode
            .clone()
            .unwrap_or(crate::config::modes::ModeName::DEFAULT),
    })
}
