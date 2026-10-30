//! Every request routed to the handler of its domain.
mod agents;
mod approvals;
mod assignments;
mod chats;
mod ci;
pub(super) mod daemon;
mod demos;
mod executions;
mod files;
mod holds;
mod items;
mod notifications;
mod ports;
mod questions;
mod queue;
mod recovery;
mod removal;
mod repositories;
mod resources;
mod secrets;
mod simulators;
pub(super) mod workspaces;
use anyhow::Result;

use super::{Server, answer::Answer, scope::Caller};
use crate::protocol::Operation;

/// Answer a request; a failure becomes an error reply.
pub(super) async fn operation(
    server: &Server,
    operation: Operation,
    caller: Option<&Caller>,
) -> Answer {
    handle(server, operation, caller)
        .await
        .unwrap_or_else(|error| Answer::failed(&error))
}

/// Forward a request to its domain's handler.
pub(super) async fn handle(
    server: &Server,
    operation: Operation,
    caller: Option<&Caller>,
) -> Result<Answer> {
    match operation {
        Operation::Daemon(method) => daemon::handle(server, method).await,
        Operation::Repository(method) => {
            repositories::handle(&server.manager, method, caller).await
        }
        Operation::Workspace(method) => workspaces::handle(&server.manager, method, caller).await,
        Operation::Removal(method) => removal::handle(&server.manager, method, caller).await,
        Operation::Recovery(method) => recovery::handle(&server.manager, method, caller).await,
        Operation::Execution(method) => executions::handle(&server.manager, method, caller).await,
        Operation::Assignment(method) => assignments::handle(&server.manager, method, caller).await,
        Operation::Agent(method) => agents::handle(&server.manager, method, caller).await,
        Operation::Chat(method) => chats::handle(&server.manager, method, caller).await,
        Operation::Queue(method) => queue::handle(&server.manager, method, caller).await,
        Operation::Hold(method) => holds::handle(&server.manager, method, caller).await,
        Operation::Demo(method) => demos::handle(&server.manager, method).await,
        Operation::File(method) => files::handle(&server.manager, method, caller).await,
        Operation::Item(method) => items::handle(&server.manager, method, caller).await,
        Operation::Ci(method) => ci::handle(&server.manager, method, caller).await,
        Operation::Notification(method) => notifications::handle(&server.manager, method).await,
        Operation::Question(method) => questions::handle(&server.manager, method, caller).await,
        Operation::Approval(method) => approvals::handle(&server.manager, method, caller).await,
        Operation::Port(method) => ports::handle(&server.manager, method, caller).await,
        Operation::Secret(method) => secrets::handle(&server.manager, method, caller).await,
        Operation::Resource(method) => resources::handle(&server.manager, method, caller).await,
        Operation::Simulator(method) => simulators::handle(&server.manager, method, caller).await,
    }
}
