//! The simulator leases and resource permits an execution acquired, which
//! end with it: who acquires a lease, and releasing an execution's leases
//! through each kind's release path once it ends.
use shoal_ids::execution_ids::ExecutionId;

#[cfg(test)]
use super::allocation::Allocation;
use super::{log, scope::Caller, workspace::Manager};
use shoal_protocol::acquirers::Acquirer;

impl Manager {
    /// Whose lease a request from `caller` acquires: the demo its execution
    /// runs, else the agent it runs or that registered it, else the user's,
    /// as for an unscoped caller or a command no agent started.
    pub(crate) async fn acquirer(&self, caller: Option<&Caller>) -> Acquirer {
        let Some(caller) = caller else {
            return Acquirer::User;
        };
        if let Some(execution) = caller.execution_id()
            && self.runs_demo(execution).await
        {
            return Acquirer::Demo {
                execution_id: execution.to_owned(),
            };
        }
        match self.caller_agent(caller).await {
            Some(agent) => Acquirer::Agent {
                execution_id: agent.execution,
            },
            None => Acquirer::User,
        }
    }

    /// Whether `execution` runs a demo; one that cannot be read is taken
    /// for none, so its lease falls to the agent that started it.
    async fn runs_demo(&self, execution: &ExecutionId) -> bool {
        let id = execution.to_owned();
        let demo = self
            .store
            .read(move |db| super::workspace::demos::runs_demo(db, &id))
            .await;
        demo.unwrap_or_else(|error| {
            log!("demo of execution {execution} unreadable: {error:#}");
            false
        })
    }
}

#[cfg(test)]
mod tests;
