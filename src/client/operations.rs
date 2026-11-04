//! Following an operation, work the daemon runs in a worker of its own, such
//! as preparing a workspace it created: its progress as it comes, then how it
//! ended. The CLI follows its operations the same way.
use anyhow::Result;
use tokio::{io::BufReader, net::UnixStream};

use crate::client::{
    connection::{self, Daemon},
    protocol::{
        Body, OperationMethod, Responses,
        operations::{Ended, OperationEvent},
    },
};

/// What following an operation reports.
#[derive(Debug, Clone)]
pub enum Followed {
    /// A step it reached or a line its processes wrote.
    Event(OperationEvent),
    /// How it ended; nothing follows.
    Ended(Ended),
}

/// An operation being followed, on a connection of its own.
pub struct Following {
    stream: Responses<BufReader<UnixStream>>,
}

impl Following {
    /// Follow operation `id` from its `after`th event, failing at once when
    /// the daemon refuses.
    pub async fn open(daemon: &impl Daemon, id: String, after: u64) -> Result<Self> {
        let method = OperationMethod::FollowOperation { id, after };
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
