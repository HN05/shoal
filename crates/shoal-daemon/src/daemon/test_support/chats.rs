//! Chats whose agents run in the test process, as their wrappers would
//! register and track them.
use anyhow::{Context as _, Result};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_spaces::chats::NewChat;

use crate::{daemon::manager::Manager, model::Chat};

impl Manager {
    /// Record the chat `name` in the repository's checkout, at the top, with
    /// its agent running.
    pub async fn start_chat(
        self: &std::sync::Arc<Self>,
        repository: crate::repository::Selector,
        name: SpaceName,
        agent: crate::agent::Agent,
        origin: Option<crate::sessions::Origin>,
    ) -> Result<Chat> {
        let new = NewChat {
            priority: None,
            narrowing: Default::default(),
            name,
            agent,
            mode: crate::model::chat_mode(),
            origin: origin.clone(),
            parent: None,
        };
        let chat = self
            .spaces
            .start_new_chat(&**self, Some(repository), None, new)
            .await?;
        self.run_chat_agent(chat, origin).await
    }

    /// [`Self::start_chat`] in the worktree of the workspace `workspace`
    /// names, which must be ready, in the workspace's repository.
    #[cfg(test)]
    pub async fn start_chat_in_workspace(
        self: &std::sync::Arc<Self>,
        workspace: &SpaceSelector,
        name: SpaceName,
        agent: crate::agent::Agent,
        origin: Option<crate::sessions::Origin>,
    ) -> Result<Chat> {
        let workspace = self.spaces.chat_workspace(&**self, workspace).await?;
        let new = NewChat {
            priority: None,
            narrowing: Default::default(),
            name,
            agent,
            mode: crate::model::chat_mode(),
            origin: origin.clone(),
            parent: None,
        };
        let repository = workspace.repository_id.clone();
        let chat = (self
            .spaces
            .record_new_chat(&**self, repository, Some(&workspace), new))
        .await?;
        self.run_chat_agent(chat, origin).await
    }

    /// Run the agent of `chat` as its execution at `origin`, as its wrapper
    /// registers and tracks it, and return the chat running it. Asked to
    /// stop, the agent ends its processes and exits, as its wrapper stops
    /// it, first saving itself for `shoal resume` when the stop asks that.
    pub(crate) async fn run_chat_agent(
        self: &std::sync::Arc<Self>,
        chat: Chat,
        origin: Option<crate::sessions::Origin>,
    ) -> Result<Chat> {
        let place = origin.map(|origin| crate::daemon::places::ExecutionPlace {
            origin,
            agent: true,
        });
        let request = crate::daemon::executions::ExecutionRequest {
            place,
            agent: true,
            ..Default::default()
        };
        let selector = SpaceSelector::id(&chat.id);
        let kind = crate::daemon::executions::ExecutionKind::Command;
        let mut started = (self
            .agents
            .begin_execution_placed(&**self, &selector, None, kind, None, request))
        .await?;
        let agent = shoal_protocol::executions::TrackedAgent {
            name: chat.agent.clone().context("the chat names no agent")?,
            model: Default::default(),
            mode: chat.agent_mode(),
            narrowing: chat.narrowing,
            new_session: false,
        };
        let id = started.plan.id.clone();
        let running = std::time::Duration::ZERO;
        (self
            .agents
            .track_agent(&**self, &id, &agent, &chat.id, &chat.name, true, running))
        .await;
        let (manager, space) = (self.clone(), chat.id.clone());
        tokio::spawn(async move {
            if started.stop.wait_for(|stop| *stop).await.is_ok() {
                let request = manager.agents.executions().stop_request(&id).await;
                if let Some(request) = request.filter(|request| request.saves(true)) {
                    use crate::execution::recovery::{self, RecordExt};
                    let stage = shoal_protocol::executions::Stage::Stopped;
                    let record = recovery::Record::new(&agent, Some(&request.reason), stage, None);
                    recovery::save_stopped(&manager.paths, &space, &id, record, None).unwrap();
                }
                manager.agents.end_test_execution(&*manager, id).await;
            }
        });
        self.spaces
            .chat_by_id(&chat.id)
            .await?
            .context("the chat was removed")
    }
}
