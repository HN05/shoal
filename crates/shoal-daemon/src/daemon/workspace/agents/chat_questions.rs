//! Questions an agent asks the user in its conversation: it marks the turn,
//! and the turn's final message reaches the user's questions too, until the
//! user answers there or replies in the conversation. An agent marks the
//! turn of the execution it runs as, a chat's as a workspace's.
use anyhow::{Context, Result, ensure};
use shoal_ids::execution_ids::ExecutionId;

use super::Manager;
use crate::{daemon::questions::set_chat_mark, model::Space};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Mark the current turn of the agent `execution` runs in the space
    /// `selector` names as asking the user in its conversation, which needs
    /// Shoal's hook that ends the agent's turns.
    pub(crate) async fn mark_chat_question(
        &self,
        selector: &SpaceSelector,
        execution: Option<&ExecutionId>,
    ) -> Result<()> {
        let space = self.space(selector).await?;
        let (turn, agent) = self.agent_turn(&space, execution).await?;
        let settings = self.present_space_settings(&space).await?;
        ensure!(
            settings.integration(&agent).reports_turn_ends(),
            "agent {agent} has no Shoal hook that ends its turns; ask with shoal questions ask \"<decision>\" --recommend <answer>"
        );
        self.store
            .run(move |db| set_chat_mark(db, &turn, true))
            .await?;
        Ok(())
    }

    /// The execution `execution` whose turn an agent of `space` marks, with
    /// that agent, refusing an execution that runs no agent of the space.
    async fn agent_turn(
        &self,
        space: &Space,
        execution: Option<&ExecutionId>,
    ) -> Result<(ExecutionId, crate::agent::Agent)> {
        let refusal = || {
            format!(
                "only an agent Shoal runs in {} asks in its chat",
                space.name
            )
        };
        let execution = execution.with_context(refusal)?.to_owned();
        let agent = self
            .executions
            .with_agent(&execution, |agent| {
                (agent.workspace_id == space.id).then(|| agent.name.clone())
            })
            .await
            .flatten()
            .with_context(refusal)?;
        Ok((execution, agent))
    }

    /// Ask the user the question in `final_message` when the agent
    /// `execution` runs in the space `selector` names marked the turn that
    /// ended as asking one. The agent's turn ends either way, so a failure
    /// is only logged.
    pub(crate) async fn ask_marked_chat_question(
        &self,
        selector: &SpaceSelector,
        execution: Option<&ExecutionId>,
        final_message: Option<String>,
    ) {
        let Some(execution) = execution else {
            return;
        };
        let asked = self.ask_in_chat(selector, execution.to_owned(), final_message.as_deref());
        if let Err(error) = asked.await {
            crate::daemon::log!("question {selector} asked in its chat not recorded: {error:#}");
        }
    }

    /// The user replied in the conversation of the agent `execution` runs in
    /// the space `selector` names: record their interaction with it, close
    /// the questions the space's agents asked in their conversations, and
    /// the mark of a turn underway, and return the closed questions'
    /// numbers.
    pub(crate) async fn reply_in_chat(
        &self,
        selector: &SpaceSelector,
        execution: Option<&ExecutionId>,
    ) -> Result<Vec<crate::daemon::item_numbers::ItemNumber>> {
        self.note_interaction(selector).await;
        (self.close_chat_questions(selector, execution.cloned())).await
    }
}

#[cfg(test)]
mod tests;
