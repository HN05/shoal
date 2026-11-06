//! Subscriptions: a snapshot of the records of some topics, then their
//! changes, on a connection of their own. A daemon restart closes the
//! connection; the subscription subscribes again and reports the new
//! snapshot as a resync.
pub mod records;

use anyhow::Result;
use tokio::{io::BufReader, net::UnixStream};

use crate::client::{
    connection::{self, Daemon},
    protocol::{Body, Responses, SubscriptionMethod},
};
use records::{Record, RecordKey, Topic};

/// What a subscription reports.
#[derive(Debug, Clone)]
pub enum Update {
    /// Every record the subscription covers, as it is now. `resynced` marks
    /// a snapshot after a reconnect, which replaces everything known before:
    /// a record it lacks was removed meanwhile.
    Snapshot {
        records: Vec<Record>,
        resynced: bool,
    },
    /// A record was added or changed.
    Changed(Box<Record>),
    /// A record was removed.
    Removed(RecordKey),
}

/// A subscription to the records of some topics.
pub struct Subscription<D> {
    daemon: D,
    topics: Vec<Topic>,
    stream: Option<Responses<BufReader<UnixStream>>>,
    /// The first snapshot, until [`Self::next`] returns it.
    first: Option<Vec<Record>>,
}

impl<D: Daemon> Subscription<D> {
    /// Subscribe to `topics` on `daemon`, failing at once when the daemon
    /// refuses.
    pub async fn open(daemon: D, topics: Vec<Topic>) -> Result<Self> {
        let (stream, records) = subscribe(&daemon, &topics).await?;
        Ok(Self {
            daemon,
            topics,
            stream: Some(stream),
            first: Some(records),
        })
    }

    /// The next update: first the snapshot, then each change, and a new
    /// snapshot once the subscription reconnected after a daemon restart.
    /// Cancelling it, as `tokio::select!` does, loses no update.
    pub async fn next(&mut self) -> Result<Update> {
        if let Some(records) = self.first.take() {
            return Ok(Update::Snapshot {
                records,
                resynced: false,
            });
        }
        loop {
            let Some(stream) = &mut self.stream else {
                let (stream, records) = resubscribe(&self.daemon, &self.topics).await?;
                self.stream = Some(stream);
                return Ok(Update::Snapshot {
                    records,
                    resynced: true,
                });
            };
            match stream.next().await {
                Ok(response) => {
                    return match response.body.into_result()? {
                        Body::RecordChanged(record) => Ok(Update::Changed(Box::new(record))),
                        Body::RecordRemoved(key) => Ok(Update::Removed(key)),
                        body => Err(body.unexpected("RecordChanged")),
                    };
                }
                Err(error) if connection::stream_closed(&error) => self.stream = None,
                Err(error) => return Err(error),
            }
        }
    }
}

/// Subscribe on a new connection and read the whole snapshot.
async fn subscribe(
    daemon: &impl Daemon,
    topics: &[Topic],
) -> Result<(Responses<BufReader<UnixStream>>, Vec<Record>)> {
    let method = SubscriptionMethod::Subscribe {
        topics: topics.to_vec(),
    };
    let (stream, body) = connection::open_buffered(daemon, method).await?;
    Ok((Responses::new(stream), Vec::try_from(body)?))
}

/// [`subscribe`] again, giving a restarting daemon time to come back and
/// know a scoped caller once its wrapper reattaches.
async fn resubscribe(
    daemon: &impl Daemon,
    topics: &[Topic],
) -> Result<(Responses<BufReader<UnixStream>>, Vec<Record>)> {
    connection::after_restart(|| subscribe(daemon, topics)).await
}
