//! Copies of spaces that keep running: a workspace at a checkpoint with its
//! agents and the sessions they reported, or a chat with its agents, which
//! `shoal move import` opens as a new space whose agents start from forks.
use crate::transfer::ManifestExt;
use anyhow::{Result, ensure};

use super::Prepared;
use crate::{
    daemon::{events::EventCause, workspace::Manager},
    execution::recovery::{self, Copied, Record, StoppedAgent},
    model::{Chat, Execution, PresentSpace, Workspace},
    time::UnixTime,
    transfer::Manifest,
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// The workspace or running chat `selector` names, refusing a removed
    /// space, which runs nothing to keep.
    pub(super) async fn running_space(&self, selector: &SpaceSelector) -> Result<PresentSpace> {
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

    /// A copy of `workspace` at its checkpoint `number`, or at a new one,
    /// with its agents, which start from forks of the sessions they
    /// reported. An earlier checkpoint marks those sessions as newer than
    /// its files.
    pub(super) async fn copy_workspace(
        &self,
        workspace: Workspace,
        number: Option<CheckpointNumber>,
    ) -> Result<Prepared> {
        let checkpoint = match number {
            Some(number) => self.numbered_checkpoint(&workspace, number).await?,
            None => {
                self.take_checkpoint(&SpaceSelector::id(&workspace.id), None)
                    .await?
            }
        };
        ensure!(
            !checkpoint.files_omitted,
            "checkpoint {} of {} could not keep the uncommitted files, so a copy of it would lose them",
            checkpoint.number,
            workspace.name
        );
        let agents = self.copied_agents(&workspace.id).await?;
        let copied = super::packaged_record(&self.paths, &workspace, checkpoint, agents);
        let repo = self.repository(workspace.repository()).await?;
        let mut manifest = Manifest::of(Some(&repo), copied).await?;
        manifest.copy = Some(Copied {
            earlier_files: number.is_some(),
            held_elsewhere: false,
        });
        Ok(Prepared {
            settings: Some(self.workspace_settings(&workspace).await),
            repo: Some(repo),
            manifest,
            removal: None,
            kept_sessions: Vec::new(),
        })
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

    /// A copy of `chat`, as its removal would record it now, with its agents
    /// as a workspace's copy has them, which start from forks of their
    /// sessions.
    pub(super) async fn copy_chat(&self, chat: Chat) -> Result<Prepared> {
        let mut record = (self.chat_removal_record(&chat.id, EventCause::Manual)).await?;
        record.agents = self.copied_agents(&chat.id).await?;
        ensure!(
            record.has_session(),
            "the agent of chat {} reported no session to fork",
            chat.name
        );
        let repo = self.chat_repository(&chat).await?;
        let mut manifest = Manifest::of(repo.as_ref(), record).await?;
        manifest.copy = Some(Copied::default());
        Ok(Prepared {
            repo,
            manifest,
            removal: None,
            settings: None,
            kept_sessions: Vec::new(),
        })
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
        model: execution.model.clone(),
        mode: execution
            .mode
            .clone()
            .unwrap_or(crate::config::modes::ModeName::DEFAULT),
    })
}
