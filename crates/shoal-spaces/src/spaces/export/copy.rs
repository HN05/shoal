//! Copies of spaces that keep running, which `shoal restore --from` opens
//! as new spaces whose agents start from forks.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::EventCause;
use anyhow::Result;

use super::Prepared;
use crate::{
    execution::recovery::{self, Copied, Record, StoppedAgent},
    model::{Execution, RemovedSpace, Space},
    time::UnixTime,
    transfer::Carrying,
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    /// Leaves them running here. A workspace is copied at its checkpoint
    /// `checkpoint`, or at a new one, and those below at new ones. `file`
    /// must be new.
    pub async fn copy_space(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        checkpoint: Option<CheckpointNumber>,
        file: std::path::PathBuf,
        carrying: &Carrying,
    ) -> Result<crate::transfer::ExportedSpace> {
        shoal_daemon_base::writable::ensure_writable(crate::transfer::directory(&file)).await?;
        let space = self.running_space(selector).await?;
        let copied = self
            .prepare_copy(daemon, space, checkpoint, carrying)
            .await?;
        self.write_prepared(daemon, copied, file).await
    }

    /// Refuses a removed space, which runs nothing to keep.
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

    /// Records each as [`Self::record_now`] does. An earlier checkpoint marks
    /// the sessions as newer than its files.
    async fn prepare_copy(
        &self,
        daemon: &dyn Daemon,
        space: Space,
        number: Option<CheckpointNumber>,
        carrying: &Carrying,
    ) -> Result<Prepared> {
        let carried = self.carried_by(daemon, &space.id, carrying).await?;
        let copied = self.record_now(daemon, &space, number).await?;
        let mut records = Vec::new();
        for below in &carried {
            records.push(self.record_now(daemon, below, None).await?);
        }
        // Each one's own config may describe its agents, and goes with it.
        let settings = self
            .agent_settings_of(daemon, std::iter::once(&space).chain(&carried))
            .await;
        let mut prepared = Prepared::of(daemon, copied, records, settings).await?;
        prepared.manifest.copy = Some(Copied {
            earlier_files: number.is_some(),
            held_elsewhere: false,
        });
        Ok(prepared)
    }

    /// As its removal would record it now, with its agents and the sessions
    /// they reported. A workspace is at its checkpoint `number`, or at a new
    /// one.
    pub(super) async fn record_now(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        number: Option<CheckpointNumber>,
    ) -> Result<RemovedSpace> {
        let checkpoint = self.copy_checkpoint(daemon, space, number).await?;
        let mut record = self
            .present_removal_record(daemon, &space.id, checkpoint, EventCause::Manual)
            .await?;
        record.agents = self.agents_now(&space.id).await?;
        // The package carries its sessions' files from where its agents keep
        // them.
        record.session_files = Vec::new();
        Ok(record)
    }

    /// Those stopped first, then those its executions run.
    pub(super) async fn agents_now(&self, id: &SpaceId) -> Result<Vec<StoppedAgent>> {
        let mut agents = recovery::stopped_agents(&self.paths, id)?;
        let now = UnixTime::now();
        for execution in self.workspace_executions(id).await? {
            if agents.iter().any(|agent| agent.execution == execution.id) {
                continue;
            }
            let carried = recovery::carried::continued(&self.paths, id, &execution.id);
            let Some(record) = running_record(&execution, carried) else {
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

/// `None` for an execution without an agent.
fn running_record(execution: &Execution, carried: recovery::carried::Carried) -> Option<Record> {
    Some(
        carried.kept_in(Record {
            narrowing: execution.narrowing,
            // Named as recovery records name it.
            agent: execution.agent.as_ref()?.normalized(),
            stop_reason: None,
            stage: None,
            origin: None,
            waiting: None,
            session: None,
            moved: false,
            copied: None,
            converted: false,
            handoff: None,
            turn: None,
            for_load: false,
            oom_kills: 0,
            restores_after: None,
            cause: None,
            model: execution.model.clone(),
            mode: execution
                .mode
                .clone()
                .unwrap_or(crate::config::modes::ModeName::DEFAULT),
        }),
    )
}
