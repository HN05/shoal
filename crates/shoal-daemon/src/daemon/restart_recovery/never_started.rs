//! Executions that never started their command, such as a setup whose
//! wrapper left before it launched it: unknown, with no recovery record and
//! neither a child nor a process group recorded. Once the wrapper is provably gone, its PID free or
//! taken by a process of another birth, and no process of the execution
//! survives, nothing of it runs, so the daemon forgets it.
use anyhow::Result;

use super::{Manager, Pending, Waiting};
use crate::client::execution_ids::ExecutionId;
use crate::{
    daemon::log,
    model::{Execution, PresentSpace},
    process::{
        execution::Processes,
        identity::{self as process, Identity},
    },
};

/// An unknown execution of `space` that never started its command, whose
/// recorded `wrapper` decides it.
pub(super) struct Unstarted {
    space: PresentSpace,
    execution: Execution,
    wrapper: Identity,
}

/// What reconciliation concluded for an execution that never started.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// Its wrapper still runs, and may yet reattach or report its exit.
    Waiting,
    /// Nothing of it runs.
    Gone,
    /// A process of it survives, or the evidence cannot be read.
    Unresolved,
}

impl Unstarted {
    /// `execution` of `space` when it never started its command: neither a
    /// child nor a process group is recorded, since a command whose child
    /// exited before its identity was read still leaves its group, and a
    /// wrapper is, which proves whether it ended.
    pub(super) fn of(space: PresentSpace, execution: Execution) -> Option<Self> {
        if execution.child.is_some() || execution.group_id.is_some() {
            return None;
        }
        Some(Self {
            space,
            wrapper: execution.wrapper.clone()?,
            execution,
        })
    }

    pub(super) fn id(&self) -> ExecutionId {
        self.execution.id.clone()
    }
}

/// Whether `processes`, inspected for an execution that never started,
/// leave it waiting for its wrapper, prove it gone, or neither.
fn assess(processes: &Processes) -> Outcome {
    if processes.wrapper.is_some() {
        Outcome::Waiting
    } else if processes.command_stopped() {
        Outcome::Gone
    } else {
        Outcome::Unresolved
    }
}

impl Manager {
    /// Forget `unstarted` once its wrapper and every process of it are gone,
    /// adding to `pending` what a later pass decides.
    pub(super) async fn reconcile_unstarted(
        &self,
        unstarted: Unstarted,
        scan: &process::Scan,
        pending: &mut Pending,
    ) -> Result<()> {
        let inspected = (self.inspect_unknown(&unstarted.space, &unstarted.execution, scan)).await;
        let outcome = match inspected {
            Ok(processes) => assess(&processes),
            Err(error) => {
                let id = &unstarted.execution.id;
                log!("execution {id} stays unknown: process inspection failed: {error:#}");
                Outcome::Unresolved
            }
        };
        match outcome {
            Outcome::Waiting => pending.waiting.push(Waiting {
                execution: unstarted.execution.id,
                wrapper: unstarted.wrapper,
            }),
            Outcome::Unresolved => pending.unresolved = true,
            Outcome::Gone => self.forget_unstarted(&unstarted).await?,
        }
        Ok(())
    }

    async fn forget_unstarted(&self, unstarted: &Unstarted) -> Result<()> {
        self.forget_execution(&unstarted.execution.id).await?;
        log!(
            "forgot execution {} in {}, which never started its command",
            unstarted.execution.id,
            unstarted.space.name()
        );
        // The execution no longer takes an agent slot.
        self.agent_queue.wake();
        Ok(())
    }
}
