//! What demos need of the rest of the daemon.
use std::collections::BTreeMap;
use std::ffi::OsString;

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_core::process::{execution::Processes, identity::Scan};
use shoal_daemon_base::callers::Caller;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::approvals::ApprovalRequest;
use shoal_protocol::model::{Execution, ExecutionSpace, PortRequest, PortReservation, Space};
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;

/// The spaces demos run in.
pub trait Spaces: Send + Sync {
    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>>;

    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>>;
}

/// The executions that run demos, and their processes.
pub trait Executions: Send + Sync {
    /// Whether the launch `launch` ended before its command ran.
    fn launch_ended_unrun(&self, launch: &str) -> bool;

    fn completing(&self, execution: &ExecutionId) -> bool;

    fn execution<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Result<Option<Execution>>>;

    /// The scope token the commands of the execution `id` act under.
    fn scope_token<'a>(&'a self, id: &'a ExecutionId) -> BoxFuture<'a, Result<ScopeToken>>;

    /// Ask the connected wrappers among `executions` to stop for `reason`,
    /// saving nothing, and return the rest.
    fn ask_to_stop(
        &self,
        executions: Vec<ExecutionId>,
        reason: String,
    ) -> BoxFuture<'_, Vec<ExecutionId>>;

    /// Stop the verified processes of `execution`, whose wrapper is away,
    /// with the proof `proof` asks for.
    fn stop_disconnected<'a>(
        &'a self,
        execution: &'a Execution,
        proof: Proof,
    ) -> BoxFuture<'a, Result<()>>;

    /// The processes of `execution` that `scan` found, with their evidence.
    fn inspect_execution<'a>(
        &'a self,
        execution: &'a Execution,
        scan: &'a Scan,
    ) -> BoxFuture<'a, Result<Processes>>;

    /// Delete the record of `execution`, whose processes were proven
    /// stopped.
    fn forget_execution<'a>(&'a self, execution: &'a ExecutionId) -> BoxFuture<'a, Result<()>>;

    /// What the space `id` gives a process that joins one of its
    /// executions, as it gives a new execution's command.
    fn joining<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Joining>>;
}

/// The proof a stop of an execution whose wrapper is away needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proof {
    /// Its records stay unless it is proven fully stopped.
    Complete,
    /// Its verified processes stop, leaving its records.
    ForRemoval,
}

/// Where a process that joins an execution of a space runs: the space as
/// the execution names it, its configured environment and the ports it
/// reserved.
pub struct Joining {
    pub space: ExecutionSpace,
    pub configured: BTreeMap<String, OsString>,
    pub ports: Vec<PortReservation>,
}

/// The ports viewers serve on.
pub trait Ports: Send + Sync {
    /// Reserve the port `name` of the workspace `workspace` as `request`
    /// asks, as `caller` reserves it.
    fn reserve_port<'a>(
        &'a self,
        workspace: &'a SpaceId,
        name: String,
        request: PortRequest,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Reserved>>;
}

/// What a port's reservation came to.
pub enum Reserved {
    /// It is reserved, as this port.
    Port(u16),
    /// The capacity it needs is busy, as the message says.
    Busy(String),
    /// It waits for the approval of this request.
    Approval(Box<ApprovalRequest>),
    /// It reserved none.
    Unreserved,
}

pub trait Daemon: Spaces + Executions + Ports {}

impl<T: Spaces + Executions + Ports> Daemon for T {}
