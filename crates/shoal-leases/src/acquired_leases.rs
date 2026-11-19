//! Who acquires a lease: the user, or the agent or demo whose execution
//! acquired it, with which it ends.
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::acquirers::Acquirer;

use crate::{Leases, callers::Caller, daemon::Daemon, log};

impl Leases {
    /// Whose lease a request from `caller` acquires: the demo its execution
    /// runs, else the agent it runs or that registered it, else the user's,
    /// as for an unscoped caller or a command no agent started.
    pub async fn acquirer(&self, daemon: &dyn Daemon, caller: Option<&Caller>) -> Acquirer {
        let Some(caller) = caller else {
            return Acquirer::User;
        };
        if let Some(execution) = caller.execution_id()
            && runs_demo(daemon, execution).await
        {
            return Acquirer::Demo {
                execution_id: execution.to_owned(),
            };
        }
        match daemon.caller_agent(caller).await {
            Some(execution_id) => Acquirer::Agent { execution_id },
            None => Acquirer::User,
        }
    }
}

/// Whether `execution` runs a demo; one that cannot be read is taken for
/// none, so its lease falls to the agent that started it.
async fn runs_demo(daemon: &dyn Daemon, execution: &ExecutionId) -> bool {
    daemon.runs_demo(execution).await.unwrap_or_else(|error| {
        log!("demo of execution {execution} unreadable: {error:#}");
        false
    })
}
