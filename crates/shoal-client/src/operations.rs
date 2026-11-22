//! Following an operation, work the daemon runs in a worker of its own, such
//! as preparing a workspace it created: its progress as it comes, then how it
//! ended. The CLI follows its operations the same way.
use std::collections::BTreeMap;

use anyhow::Result;
use tokio::{io::BufReader, net::UnixStream};

use crate::connection::{self, Daemon};
use shoal_protocol::protocol::{
    Body, OperationMethod, Responses,
    operations::{Ended, OperationEvent, OperationRef, OperationSpec, Report, Started},
};
use shoal_protocol::transfer::{DeferredMove, MoveOrder};

/// What following an operation reports, ending in `E`, how it ended.
#[derive(Debug, Clone)]
pub enum Followed<E = Ended> {
    /// A step it reached or a line its processes wrote.
    Event(OperationEvent),
    /// How it ended; nothing follows.
    Ended(E),
}

/// An operation being followed, on a connection of its own.
pub struct Following {
    stream: Responses<BufReader<UnixStream>>,
}

impl Following {
    /// Follow `operation` from its `after`th event, failing at once when
    /// the daemon refuses.
    pub async fn open(daemon: &impl Daemon, operation: OperationRef, after: u64) -> Result<Self> {
        let method = OperationMethod::FollowOperation { operation, after };
        let (stream, body) = connection::open_buffered(daemon, method).await?;
        <()>::try_from(body)?;
        Ok(Self {
            stream: Responses::new(stream),
        })
    }

    /// The next event, or the operation's end. Cancelling it, as
    /// `tokio::select!` does, loses no event. A daemon that goes away ends
    /// following with an error.
    pub async fn next(&mut self) -> Result<Followed> {
        let response = match self.stream.next().await {
            Err(error) if connection::stream_closed(&error) => {
                anyhow::bail!("lost the daemon while following the operation, which continues")
            }
            response => response?,
        };
        match response.body.into_result()? {
            Body::OperationEvent(event) => Ok(Followed::Event(event)),
            Body::OperationEnded(ended) => Ok(Followed::Ended(ended)),
            body => Err(body.unexpected("OperationEvent")),
        }
    }
}

/// How a move started.
pub enum MoveStarted {
    /// Its operation of this ID runs, followed from its start.
    Following(String, Box<Following>),
    /// It starts once the turn of the agent it would stop ends.
    AfterTurn(DeferredMove),
}

impl Following {
    /// Start the move `order` with `env` on the machine of its spaces and
    /// follow it from its start, returning its ID with it, unless it waits
    /// for the turn of the agent it would stop.
    pub async fn start_move(
        daemon: &impl Daemon,
        order: MoveOrder,
        env: BTreeMap<String, String>,
    ) -> Result<MoveStarted> {
        let method = OperationMethod::StartMove {
            order: Box::new(order),
            env: Some(env),
        };
        let (stream, body) = connection::open_buffered(daemon, method).await?;
        if let Body::MoveAfterTurn(deferred) = body {
            return Ok(MoveStarted::AfterTurn(deferred));
        }
        let Started { id } = Started::try_from(body)?;
        let stream = Responses::new(stream);
        Ok(MoveStarted::Following(id, Box::new(Self { stream })))
    }
}

/// The move `order` that [`Following::start_move`] starts, as the picker
/// that chooses its spaces judges it.
pub fn move_request(order: MoveOrder) -> connection::Prepared<Started> {
    connection::Prepared::new(OperationMethod::StartMove {
        order: Box::new(order),
        env: None,
    })
}

/// Start `spec` with `env`, returning the started operation's ID to follow.
pub async fn start(
    daemon: &impl Daemon,
    spec: OperationSpec,
    env: BTreeMap<String, String>,
) -> Result<String> {
    let Started { id } = start_request(spec, env).send(daemon).await?;
    Ok(id)
}

/// What [`start`] sends for `spec` with `env`, as the picker that chooses
/// the space it works in judges it.
pub fn start_request(
    spec: OperationSpec,
    env: BTreeMap<String, String>,
) -> connection::Prepared<Started> {
    connection::Prepared::new(OperationMethod::StartOperation {
        spec,
        env: Some(env),
    })
}

/// Cancel `operation`.
pub async fn cancel(daemon: &impl Daemon, operation: OperationRef) -> Result<()> {
    connection::request(daemon, OperationMethod::CancelOperation { operation }).await
}

/// Report a step of operation `id`, or its end, from its worker.
pub async fn report(daemon: &impl Daemon, id: String, report: Report) -> Result<()> {
    connection::request(daemon, OperationMethod::ReportProgress { id, report }).await
}

#[cfg(test)]
mod tests;
