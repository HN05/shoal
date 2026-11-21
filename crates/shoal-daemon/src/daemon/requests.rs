//! Every request routed to the handler of its domain.
mod acting;
mod agents;
mod alerts;
mod approvals;
mod apps;
mod assignments;
mod branches;
mod changes;
mod chats;
mod ci;
mod config;
mod containers;
pub(super) mod daemon;
mod demos;
mod diffs;
mod executions;
mod files;
mod forwarded;
mod holds;
mod hub;
mod items;
mod launches;
mod merging;
mod operations;
mod permissions;
mod places;
mod ports;
mod queue;
mod recovery;
mod removal;
mod repositories;
mod resources;
mod reviews;
mod secrets;
mod simulators;
pub(super) mod spaces;
mod subscriptions;
mod threads;
mod triggers;
mod workspaces;
use anyhow::Result;
use shoal_protocol::scope_tokens::ScopeToken;

use super::{Server, answer::Answer, scope::Caller};
use crate::protocol::Operation;

/// Answer a request from `caller`, whose scope token is `scope`.
pub(super) async fn operation(
    server: &Server,
    operation: Operation,
    caller: Option<&Caller>,
    scope: Option<&ScopeToken>,
) -> Result<Answer> {
    // Boxed, so the wrappers that serve it each hold a pointer instead of
    // the largest request's state, which overflows a debug build's worker
    // stack within a request as deep as a removal of nested spaces.
    Box::pin(handle(server, operation, caller, scope)).await
}

/// Forward a request to its domain's handler.
pub(super) async fn handle(
    server: &Server,
    operation: Operation,
    caller: Option<&Caller>,
    scope: Option<&ScopeToken>,
) -> Result<Answer> {
    match operation {
        Operation::Daemon(method) => daemon::handle(server, method).await,
        Operation::Repository(method) => {
            repositories::handle(&server.manager, method, caller).await
        }
        Operation::Workspace(method) => workspaces::handle(&server.manager, method, caller).await,
        Operation::Space(method) => spaces::handle(&server.manager, method, caller, scope).await,
        Operation::Place(method) => places::handle(&server.manager, method).await,
        Operation::Permission(method) => permissions::handle(&server.manager, method, caller).await,
        Operation::Config(method) => config::handle(&server.manager, method).await,
        Operation::Diff(method) => diffs::handle(&server.manager, method, scope).await,
        Operation::Merging(method) => merging::handle(&server.manager, method, caller).await,
        Operation::Removal(method) => removal::handle(&server.manager, method, caller).await,
        Operation::Recovery(method) => recovery::handle(&server.manager, method, caller).await,
        Operation::Execution(method) => executions::handle(&server.manager, method, caller).await,
        Operation::Assignment(method) => assignments::handle(&server.manager, method, caller).await,
        Operation::Agent(method) => agents::handle(&server.manager, method, caller).await,
        Operation::Chat(method) => chats::handle(&server.manager, method, caller).await,
        Operation::Launch(method) => launches::handle(&server.manager, method, caller).await,
        Operation::Queue(method) => queue::handle(&server.manager, method, caller).await,
        Operation::Hold(method) => holds::handle(&server.manager, method, caller).await,
        Operation::Demo(method) => demos::handle(&server.manager, method, caller, scope).await,
        Operation::File(method) => files::handle(&server.manager, method, caller).await,
        Operation::Item(method) => items::handle(&server.manager, method, caller).await,
        Operation::Ci(method) => ci::handle(&server.manager, method, caller).await,
        Operation::Branch(method) => branches::handle(&server.manager, method, caller).await,
        Operation::Alert(method) => alerts::handle(&server.manager, method, caller).await,
        Operation::Operations(method) => {
            operations::handle(&server.manager, method, caller, scope).await
        }
        Operation::Thread(method) => threads::handle(&server.manager, method, caller).await,
        Operation::Change(method) => changes::handle(&server.manager, method, caller).await,
        Operation::Review(method) => reviews::handle(&server.manager, method, caller).await,
        Operation::Approval(method) => approvals::handle(&server.manager, method, caller).await,
        Operation::Port(method) => ports::handle(&server.manager, method, caller).await,
        Operation::Secret(method) => secrets::handle(&server.manager, method, caller).await,
        Operation::Resource(method) => resources::handle(&server.manager, method, caller).await,
        Operation::Simulator(method) => simulators::handle(&server.manager, method, caller).await,
        Operation::Container(method) => containers::handle(&server.manager, method, caller).await,
        Operation::Subscription(method) => {
            subscriptions::handle(&server.manager, method, caller, scope)
        }
        Operation::Trigger(method) => triggers::handle(&server.manager, method, caller).await,
        Operation::Hub(method) => hub::handle(&server.manager, method, caller).await,
        Operation::App(method) => apps::handle(&server.manager, method, scope).await,
    }
}
