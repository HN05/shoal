//! The destination's side of a move between joined machines: the package
//! the other machine writes on the request's stream goes into
//! `shoal internal import-moved`, which continues its spaces here, their
//! claims at the hub carrying the move's ticket, and reports how it went.
use std::{process::Stdio, sync::Arc};

use anyhow::{Context as _, Result, bail};
use shoal_ids::move_tickets::MoveTicket;
use shoal_protocol::protocol::{self, Body, Response};
use tokio::net::UnixStream;

use crate::{
    daemon::{
        answer::{self, OperationGuard},
        manager::Manager,
    },
    transfer::ImportedMove,
    workers::{Worker, detached_command, internal_command},
};

impl Manager {
    /// Continue the package of a move that another joined machine writes on
    /// `stream` once this daemon answered `request_id`, its spaces' claims
    /// carrying `ticket` and their agents restored when `resume`, then
    /// answer again with how that went, holding `guard` until then.
    pub(crate) async fn receive_move(
        self: Arc<Self>,
        mut stream: UnixStream,
        request_id: u64,
        (ticket, resume): (MoveTicket, bool),
        guard: OperationGuard,
    ) -> Result<()> {
        protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
        let body = match self.import_moved(&mut stream, &ticket, resume).await {
            Ok(imported) => Body::ImportedMove(imported),
            Err(error) => answer::failure(&error),
        };
        protocol::write(&mut stream, &Response::new(request_id, body)).await?;
        drop(guard);
        Ok(())
    }

    /// How `shoal internal import-moved` continued the package `stream`
    /// carries until the caller ends its side, as the user, in this
    /// daemon's environment.
    async fn import_moved(
        &self,
        stream: &mut UnixStream,
        ticket: &MoveTicket,
        resume: bool,
    ) -> Result<ImportedMove> {
        let argv = internal_command(&self.paths, true, Worker::ImportMoved { ticket, resume })?;
        let mut command = detached_command(&argv);
        command
            .current_dir(&self.paths.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove(crate::env::EXECUTION_ID)
            .env_remove(crate::env::SCOPE_TOKEN);
        let mut child = command.spawn().context("start the import")?;
        let mut stdin = child.stdin.take().context("the import takes no input")?;
        // An import that refuses the package stops reading it, and says why.
        let feeding = async move {
            let _ = tokio::io::copy(stream, &mut stdin).await;
        };
        let (_, output) = tokio::join!(feeding, child.wait_with_output());
        reported(&output.context("run the import")?)
    }
}

/// What the import that wrote `output` reported: its last line of
/// standard output.
fn reported(output: &std::process::Output) -> Result<ImportedMove> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let last = stdout.lines().rev().find(|line| !line.trim().is_empty());
    if let Some(imported) = last.and_then(|line| serde_json::from_str(line).ok()) {
        return Ok(imported);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "the import here ended ({}) without saying how it went: {}",
        output.status,
        stderr.trim()
    )
}

#[cfg(test)]
mod tests;
