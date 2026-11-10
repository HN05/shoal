//! Copies of spaces that keep running: a workspace at a checkpoint with its
//! agents and the sessions they reported, or a chat with its session, which
//! `shoal move import` opens as a new space whose agents start from forks.
use crate::execution::recovery::LastSessionExt;
use crate::transfer::ManifestExt;
use anyhow::{Result, ensure};

use super::Prepared;
use crate::client::space_names::SpaceSelector;
use crate::model::CheckpointNumber;
use crate::{
    daemon::{events::EventCause, workspace::Manager},
    execution::recovery::{self, Copied, LastSession, Record, StoppedAgent},
    model::{Chat, Execution, PresentSpace, RemovedWorkspace, Workspace},
    time::UnixTime,
    transfer::{Manifest, Source},
};

impl Manager {
    /// The workspace or running chat `selector` names, refusing a removed
    /// workspace, which runs nothing to keep.
    pub(super) async fn running_space(&self, selector: &SpaceSelector) -> Result<PresentSpace> {
        let error = match self.space(selector).await {
            Ok(space) => return Ok(space),
            Err(error) => error,
        };
        let removed = self
            .removed_workspaces()
            .await?
            .into_iter()
            .find(|removed| selector.names(&removed.workspace_id, &removed.name));
        match removed {
            Some(removed) => anyhow::bail!(
                "workspace {name} was removed, so nothing runs to keep; export it without --keep, or shoal restore {name} opens it again",
                name = removed.name
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
        let copied = RemovedWorkspace {
            workspace_id: workspace.id.clone(),
            repository_id: workspace.repository_id.clone(),
            name: workspace.name.clone(),
            expires_at: checkpoint.expires_at,
            checkpoint,
            base_ref: workspace.base_ref.clone(),
            base_commit: workspace.base_commit.clone(),
            at: workspace.at.clone(),
            links: workspace.links.clone(),
            agents: self.copied_agents(&workspace).await?,
            // An unreadable last session is not worth refusing the copy for.
            last_sessions: LastSession::all(&self.paths, &workspace.id).unwrap_or_default(),
            session_files: Vec::new(),
            cause: EventCause::Manual,
            removed_at: UnixTime::now(),
        };
        let repo = self.repository(&workspace.repository_id).await?;
        let mut manifest = Manifest::of(&repo, Source::Workspace(copied)).await?;
        manifest.copy = Some(Copied {
            earlier_files: number.is_some(),
        });
        Ok(Prepared {
            settings: Some(self.workspace_settings(&workspace).await),
            repo,
            manifest,
            removal: None,
            kept_sessions: Vec::new(),
            continues: None,
        })
    }

    /// The agents of `workspace` a copy starts again, each with the session
    /// it reported last: those stopped, then those its executions run.
    async fn copied_agents(&self, workspace: &Workspace) -> Result<Vec<StoppedAgent>> {
        let mut agents = recovery::stopped_agents(&self.paths, &workspace.id)?;
        let now = UnixTime::now();
        for execution in self.workspace_executions(&workspace.id).await? {
            if agents.iter().any(|agent| agent.execution == execution.id) {
                continue;
            }
            let session = recovery::reported_session(&self.paths, &workspace.id, &execution.id);
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

    /// A copy of `chat`, whose agent starts from a fork of its session.
    pub(super) async fn copy_chat(&self, chat: Chat) -> Result<Prepared> {
        if let Some(workspace) = &chat.workspace {
            anyhow::bail!(
                "chat {} runs in workspace {}, and shoal move does not copy a chat in a workspace",
                chat.name,
                workspace.name
            );
        }
        ensure!(
            chat.session.is_some(),
            "the agent of chat {} reported no session to fork",
            chat.name
        );
        let Some(repo) = self.chat_repository(&chat).await? else {
            anyhow::bail!(
                "chat {} has no repository, and shoal move copies only a chat in one",
                chat.name
            );
        };
        let mut manifest = Manifest::of(&repo, Source::Chat(chat)).await?;
        manifest.copy = Some(Copied::default());
        Ok(Prepared {
            repo,
            manifest,
            removal: None,
            settings: None,
            kept_sessions: Vec::new(),
            continues: None,
        })
    }
}

/// The record a copy keeps of the agent `execution` runs, with the `session`
/// it reported; `None` for an execution without an agent.
fn running_record(
    execution: &Execution,
    session: Option<crate::client::agent::AgentSessionId>,
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
        stopped_by: None,
        turn: None,
        model: execution.model.clone(),
        mode: execution
            .mode
            .clone()
            .unwrap_or(crate::config::modes::ModeName::DEFAULT),
    })
}
