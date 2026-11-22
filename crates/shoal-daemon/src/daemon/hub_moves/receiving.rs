//! The destination's side of a move between joined machines: the package
//! the other machine writes on the request's stream goes into
//! `shoal internal import-moved`, which continues its spaces here, their
//! claims at the hub carrying the move's ticket, and reports how it went;
//! a removed space's package the daemon records as removed itself, since
//! nothing of it opens.
use std::{process::Stdio, sync::Arc};

use anyhow::{Context as _, Result, bail};
use shoal_ids::move_tickets::MoveTicket;
use shoal_protocol::protocol::{self, Body, Response};
use tokio::net::UnixStream;

use crate::{
    daemon::{
        alerts::AlertKind,
        answer::{self, OperationGuard},
        log,
        manager::Manager,
        restore::PackageInput,
    },
    model::Space,
    refusal::Reason,
    transfer::ImportedMove,
    workers::{Worker, detached_command, internal_command},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// A move another joined machine delivers here.
pub(crate) struct Receiving {
    /// The ticket its claims carry, which a copy lacks.
    pub(crate) ticket: Option<MoveTicket>,
    /// Whether its agents' restores are queued here.
    pub(crate) resume: bool,
    /// The name of the machine it moves from.
    pub(crate) from: String,
    /// Whether it moves a removed space's record, which stays removed here.
    pub(crate) removed: bool,
}

impl Manager {
    /// Continue the package of the move `receiving` that another joined
    /// machine writes on `stream` once this daemon answered `request_id`,
    /// telling the agents of the spaces a move, not a copy, brought where
    /// they are now, or record a removed space's package as removed here,
    /// then answer again with how that went, holding `guard` until then.
    pub(crate) async fn receive_move(
        self: Arc<Self>,
        mut stream: UnixStream,
        request_id: u64,
        receiving: Receiving,
        guard: OperationGuard,
    ) -> Result<()> {
        protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
        if receiving.removed {
            return self
                .receive_removed(stream, request_id, receiving.ticket, guard)
                .await;
        }
        let ticket = receiving.ticket.as_ref();
        let body = match self
            .import_moved(&mut stream, ticket, receiving.resume)
            .await
        {
            Ok(imported) => {
                // A copy's agents start as copies, while the originals run on.
                if let ImportedMove::Arrived { arrived, carried } = &imported
                    && ticket.is_some()
                {
                    let arrived = std::iter::once(&arrived.space.id).chain(carried);
                    self.tell_arrived(arrived, &receiving.from).await;
                }
                Body::ImportedMove(imported)
            }
            Err(error) => answer::failure(&error),
        };
        protocol::write(&mut stream, &Response::new(request_id, body)).await?;
        drop(guard);
        Ok(())
    }

    /// Record the package of a removed space that another joined machine
    /// writes on `stream` as removed here, its claims at the hub carrying
    /// `ticket`, then answer `request_id` with how that went, holding
    /// `guard` until then.
    async fn receive_removed(
        &self,
        stream: UnixStream,
        request_id: u64,
        ticket: Option<MoveTicket>,
        guard: OperationGuard,
    ) -> Result<()> {
        let replying = stream.into_std()?;
        let input = PackageInput::Connection(replying.try_clone()?);
        let imported = self.record_removed(input, ticket).await;
        replying.set_nonblocking(true)?;
        let mut stream = UnixStream::from_std(replying)?;
        let body = Body::ImportedMove(imported);
        protocol::write(&mut stream, &Response::new(request_id, body)).await?;
        drop(guard);
        Ok(())
    }

    /// How recording the removed spaces of the package `input` holds as
    /// removed here went, their claims at the hub carrying `ticket`.
    async fn record_removed(
        &self,
        input: PackageInput,
        ticket: Option<MoveTicket>,
    ) -> ImportedMove {
        // Only its claim at the hub lets its source forget it.
        let Some(ticket) = ticket else {
            return ImportedMove::Refused {
                error: "a removed space moves with a ticket".to_owned(),
            };
        };
        match self.import_removed(input, ticket).await {
            Ok(carried) => ImportedMove::Recorded { carried },
            Err(error) if Reason::of(&error) == Some(Reason::NotImported) => {
                ImportedMove::Refused {
                    error: format!("{error:#}"),
                }
            }
            Err(error) => ImportedMove::Failed {
                error: format!("{error:#}"),
            },
        }
    }

    /// How `shoal internal import-moved` continued the package `stream`
    /// carries until the caller ends its side, as the user, in this
    /// daemon's environment.
    async fn import_moved(
        &self,
        stream: &mut UnixStream,
        ticket: Option<&MoveTicket>,
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

impl Manager {
    /// Tell the agents of the `arrived` spaces, which moved here from the
    /// machine named `from`, where they are now.
    async fn tell_arrived(&self, arrived: impl Iterator<Item = &SpaceId>, from: &str) {
        for id in arrived {
            let told = async {
                let space = self.space(&SpaceSelector::id(id)).await?;
                let text = arrived_message(&space, from);
                self.agents
                    .queue_agent_message(id, AlertKind::SpaceMoved, text)
                    .await
            };
            if let Err(error) = told.await {
                log!("space {id}, which moved here from {from}, is untold: {error:#}");
            }
        }
    }
}

/// What the agent of `space`, which moved here from the machine named
/// `from`, hears as it continues here.
fn arrived_message(space: &Space, from: &str) -> String {
    let place = match space.worktree() {
        Some(workspace) => format!(", and its worktree is {}", workspace.path.display()),
        None => String::new(),
    };
    format!(
        "Your space moved here from machine {from}{place}; your session continues on this machine, so go on with your work here."
    )
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
