//! Each client API call, run as the daemon operation it corresponds to,
//! with that operation's scope check.
mod decisions;
mod items;
mod repositories;
mod resources;
mod workspaces;

use serde::Serialize;
use shoal_client::{ApiError, Call};

use super::{Session, errors};
use crate::{
    daemon::{answer::Answer, requests, scope},
    protocol::{Body, Operation},
};

/// The result of `call`.
pub(super) async fn answer(session: &Session, call: Call) -> Result<serde_json::Value, ApiError> {
    match call {
        Call::ListRepositories(_) => value(repositories::list(session).await),
        Call::SyncRepository(params) => value(repositories::sync(session, params).await),
        Call::GetWorkspace(params) => value(workspaces::get(session, params.workspace).await),
        Call::RemoveWorkspace(params) => value(workspaces::remove(session, params).await),
        Call::MarkDone(params) => value(workspaces::done(session, params).await),
        Call::UndoDone(params) => value(workspaces::undo_done(session, params).await),
        Call::RenameWorkspace(params) => value(workspaces::rename(session, params).await),
        Call::QueueAgent(params) => value(workspaces::queue_agent(session, params).await),
        Call::StopAgent(params) => value(workspaces::stop_agent(session, params).await),
        Call::AddLink(params) => value(items::link(session, params).await),
        Call::RemoveLink(params) => value(items::unlink(session, params).await),
        Call::AcquireHold(params) => value(resources::acquire_hold(session, params).await),
        Call::ReleaseHold(params) => value(resources::release_hold(session, params).await),
        Call::AcquireSimulator(params) => {
            value(resources::acquire_simulator(session, params).await)
        }
        Call::ReleaseSimulator(params) => {
            value(resources::release_simulator(session, params).await)
        }
        Call::AnswerQuestion(params) => value(decisions::answer_question(session, params).await),
        Call::DecideApproval(params) => value(decisions::decide_approval(session, params).await),
        Call::Subscribe(_) => Err(errors::invalid("a subscription is not a call")),
    }
}

fn value<T: Serialize>(result: Result<T, ApiError>) -> Result<serde_json::Value, ApiError> {
    serde_json::to_value(result?).map_err(|error| errors::unexpected(error.into()))
}

impl Session {
    /// Run `operation` as the internal protocol runs it for this caller,
    /// with its scope check, returning its reply.
    async fn run(&self, operation: impl Into<Operation>) -> Result<Body, ApiError> {
        let mut operation = operation.into();
        let manager = &self.server.manager;
        let caller = scope::authorize(manager, self.scope.as_deref(), &mut operation)
            .await
            .map_err(|error| errors::scope_refused(&error))?;
        let answer = requests::handle(&self.server, operation, caller.as_ref())
            .await
            .map_err(|error| errors::failed(&error))?;
        match answer {
            Answer::Body(body) => errors::answered(body),
            _ => Err(errors::unexpected(anyhow::anyhow!(
                "the operation answered in a form the client API does not relay"
            ))),
        }
    }

    /// [`Self::run`], reading the reply as `T`.
    async fn ask<T: TryFrom<Body, Error = anyhow::Error>>(
        &self,
        operation: impl Into<Operation>,
    ) -> Result<T, ApiError> {
        T::try_from(self.run(operation).await?).map_err(errors::unexpected)
    }
}

#[cfg(test)]
mod tests;
