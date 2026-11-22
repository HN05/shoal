//! The keys each tracked agent reads from its terminal, as its wrapper
//! reports them, which are the user's interaction with it unless Shoal
//! typed them.
use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use std::time::Duration;

use super::Manager;
use crate::time::UnixTime;

/// How long after Shoal starts typing into an agent's terminal a key the
/// agent reads is Shoal's: the access time a report gives is the second the
/// read moved it to, which follows the typing at once. A read of Shoal's
/// keys that left it unchanged reports nothing, so a later key is the
/// user's.
const TYPED_FOR: Duration = Duration::from_secs(2);

impl Manager {
    /// Record that the agent `execution` runs in the space `selector` names
    /// read a key at `at`, as the user's interaction with it unless Shoal
    /// typed it: the first read reported after Shoal started typing is
    /// Shoal's when it came within [`TYPED_FOR`], and every later one the
    /// user's. A report for an agent the
    /// daemon does not track there changes nothing.
    pub(crate) async fn report_input(
        &self,
        selector: &SpaceSelector,
        execution: &ExecutionId,
        at: UnixTime,
    ) -> Result<()> {
        let space = self.space(selector).await?;
        let shoals = (self.executions)
            .with_agent(execution, |agent| {
                (agent.workspace_id == space.id).then(|| {
                    let typed = agent.typed_at.take();
                    typed.is_some_and(|typed| (typed..=typed + TYPED_FOR).contains(&at))
                })
            })
            .await
            .flatten();
        match shoals {
            Some(false) => self.agents.record_interaction(&space.id, at).await,
            Some(true) | None => Ok(()),
        }
    }

    /// Record that Shoal types into the terminal of the agent `execution`
    /// runs in the space `selector` names now.
    pub(crate) async fn report_typing(
        &self,
        selector: &SpaceSelector,
        execution: &ExecutionId,
    ) -> Result<()> {
        let space = self.space(selector).await?;
        (self.executions)
            .with_agent(execution, |agent| {
                if agent.workspace_id == space.id {
                    agent.typed_at = Some(UnixTime::now());
                }
            })
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
