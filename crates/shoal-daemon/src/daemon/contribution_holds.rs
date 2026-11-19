//! Serves `shoal contribute`'s hold on its contribution: the registration's
//! connection, or the one its client opens again after a daemon restart,
//! keeps the contribution from cleanup until the client hangs up.
use std::sync::Arc;

use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use tokio::net::UnixStream;

use super::{
    answer::{self, OperationGuard},
    contributions::holds::ContributionHold,
    manager::Manager,
};
use crate::protocol::{self, Body, Response};

/// Register `source` as a contribution, answer with it and hold it.
pub(super) async fn register(
    stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    (source, name): (String, Option<String>),
    guard: OperationGuard,
) -> Result<()> {
    let registered = manager.register_contribution(source, name).await;
    let held = registered.map(|(repo, hold)| (Body::Repository(repo), hold));
    keep(stream, request_id, held, guard).await
}

/// Hold the registered repository `repository` again.
pub(super) async fn hold(
    stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    repository: RepositoryId,
    guard: OperationGuard,
) -> Result<()> {
    let held = manager.hold_contribution(&repository).await;
    keep(stream, request_id, held.map(|hold| (Body::Ok, hold)), guard).await
}

/// Answer with the reply of `held`, then keep its hold until the client
/// hangs up.
async fn keep(
    mut stream: UnixStream,
    request_id: u64,
    held: Result<(Body, ContributionHold)>,
    guard: OperationGuard,
) -> Result<()> {
    let (body, _hold) = match held {
        Ok(held) => held,
        Err(error) => {
            let response = Response::new(request_id, answer::failure(&error));
            return protocol::write(&mut stream, &response).await;
        }
    };
    protocol::write(&mut stream, &Response::new(request_id, body)).await?;
    drop(guard);
    answer::hangup(&mut stream).await;
    Ok(())
}
