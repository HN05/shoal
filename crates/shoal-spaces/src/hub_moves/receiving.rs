//! The destination's side of a move between joined machines: the package
//! goes into `shoal internal import-moved`, which continues its spaces
//! here. A removed space's package the daemon records as removed itself,
//! since nothing of it opens.
use crate::Spaces;
use crate::daemon::Daemon;
use std::process::Stdio;

use anyhow::{Context as _, Result, bail};
use shoal_ids::move_tickets::MoveTicket;
use shoal_protocol::protocol::{self, Body, Response};
use tokio::net::UnixStream;

use crate::{
    model::Space,
    refusal::Reason,
    transfer::{Claim, ImportedMove},
    workers::{Worker, detached_command, internal_command},
    {alerts::AlertKind, frames::OperationGuard, log, restore::PackageInput},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

pub struct Receiving {
    /// The ticket its claims carry, which a copy lacks.
    pub ticket: Option<MoveTicket>,
    pub resume: bool,
    pub from: String,
    /// It moves a removed space's record, which stays removed here.
    pub removed: bool,
}

impl Spaces {
    /// Answers `request_id` first, then again with how it went, holding
    /// `guard` until then. Tells the agents of the spaces a move, not a
    /// copy, brought where they are now.
    pub async fn receive_move(
        &self,
        daemon: &dyn Daemon,
        mut stream: UnixStream,
        request_id: u64,
        receiving: Receiving,
        guard: OperationGuard,
    ) -> Result<()> {
        protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
        if receiving.removed {
            return self
                .receive_removed(daemon, stream, request_id, receiving.ticket, guard)
                .await;
        }
        let claim = match &receiving.ticket {
            Some(ticket) => Claim::Move(ticket.clone()),
            None => Claim::Plain,
        };
        let body = match self
            .import_moved(Package::Stream(&mut stream), &claim, receiving.resume)
            .await
        {
            Ok(imported) => {
                // A copy's agents start as copies, while the originals run
                // on.
                if let ImportedMove::Arrived { arrived, carried } = &imported
                    && receiving.ticket.is_some()
                {
                    let arrived = std::iter::once(&arrived.space.id).chain(carried);
                    self.tell_arrived(daemon, arrived, &receiving.from).await;
                }
                Body::ImportedMove(imported)
            }
            Err(error) => daemon.failure(&error),
        };
        protocol::write(&mut stream, &Response::new(request_id, body)).await?;
        drop(guard);
        Ok(())
    }

    /// The claims at the hub carry `ticket`. Answers `request_id` with how
    /// that went, holding `guard` until then.
    async fn receive_removed(
        &self,
        daemon: &dyn Daemon,
        stream: UnixStream,
        request_id: u64,
        ticket: Option<MoveTicket>,
        guard: OperationGuard,
    ) -> Result<()> {
        let replying = stream.into_std()?;
        let input = PackageInput::Connection(replying.try_clone()?);
        let imported = self.record_removed(daemon, input, ticket).await;
        replying.set_nonblocking(true)?;
        let mut stream = UnixStream::from_std(replying)?;
        let body = Body::ImportedMove(imported);
        protocol::write(&mut stream, &Response::new(request_id, body)).await?;
        drop(guard);
        Ok(())
    }

    /// The claims at the hub carry `ticket`.
    async fn record_removed(
        &self,
        daemon: &dyn Daemon,
        input: PackageInput,
        ticket: Option<MoveTicket>,
    ) -> ImportedMove {
        // Only its claim at the hub lets its source forget it.
        let Some(ticket) = ticket else {
            return ImportedMove::Refused {
                error: "a removed space moves with a ticket".to_owned(),
            };
        };
        match self.import_removed(daemon, input, ticket).await {
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

    /// Claims the spaces at the hub as `claim` says. Runs as the user, in
    /// this daemon's environment.
    pub(super) async fn import_moved(
        &self,
        input: Package<'_>,
        claim: &Claim,
        resume: bool,
    ) -> Result<ImportedMove> {
        let argv = internal_command(&self.paths, true, Worker::ImportMoved { claim, resume })?;
        let mut command = detached_command(&argv);
        let stdin = match &input {
            Package::Stream(_) => Stdio::piped(),
            Package::File(file) => Stdio::from(file.try_clone()?),
        };
        command
            .current_dir(&self.paths.home)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove(crate::env::EXECUTION_ID)
            .env_remove(crate::env::SCOPE_TOKEN);
        let mut child = command.spawn().context("start the import")?;
        let stdin = child.stdin.take();
        // An import that refuses the package stops reading it, and says why.
        let feeding = async move {
            if let (Package::Stream(stream), Some(mut stdin)) = (input, stdin) {
                let _ = tokio::io::copy(stream, &mut stdin).await;
            }
        };
        let (_, output) = tokio::join!(feeding, child.wait_with_output());
        reported(&output.context("run the import")?)
    }
}

pub(super) enum Package<'a> {
    /// A request's stream, until the caller ends its side.
    Stream(&'a mut UnixStream),
    File(std::fs::File),
}

impl Spaces {
    pub(super) async fn tell_arrived(
        &self,
        daemon: &dyn Daemon,
        arrived: impl Iterator<Item = &SpaceId>,
        from: &str,
    ) {
        for id in arrived {
            let told = async {
                let space = self.space(&SpaceSelector::id(id)).await?;
                let text = arrived_message(&space, from);
                daemon
                    .queue_agent_message(id, AlertKind::SpaceMoved, text)
                    .await
            };
            if let Err(error) = told.await {
                log!("space {id}, which moved here from {from}, is untold: {error:#}");
            }
        }
    }
}

fn arrived_message(space: &Space, from: &str) -> String {
    let place = match space.worktree() {
        Some(workspace) => format!(", and its worktree is {}", workspace.path.display()),
        None => String::new(),
    };
    format!(
        "Your space moved here from machine {from}{place}; your session continues on this machine, so go on with your work here."
    )
}

/// Its last line of standard output.
pub fn reported(output: &std::process::Output) -> Result<ImportedMove> {
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
