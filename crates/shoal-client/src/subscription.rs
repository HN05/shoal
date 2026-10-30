//! Subscriptions: a snapshot of the records of some topics, then their
//! changes. A daemon restart closes the connection; the subscription
//! reconnects and reports the new snapshot as a resync.
use std::time::Duration;

use crate::{
    client::{Connection, Connector, subscribe_call},
    errors::Error,
    methods::Topic,
    records::{Record, RecordKey},
    wire::{Event, Reply},
};

/// The first wait before reconnecting; it doubles up to [`RECONNECT_MAX`].
const RECONNECT_INITIAL: Duration = Duration::from_millis(100);
const RECONNECT_MAX: Duration = Duration::from_secs(1);
/// How long a reconnect keeps trying before [`Subscription::next`] fails:
/// long enough for the service manager to start the daemon again.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(120);

/// What a subscription reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// Every record the subscription covers, as it is now. `resynced` marks
    /// a snapshot after a reconnect, which replaces everything known before:
    /// a record it lacks was removed meanwhile.
    Snapshot {
        records: Vec<Record>,
        resynced: bool,
    },
    /// A record was added or changed.
    Changed(Record),
    /// A record was removed.
    Removed(RecordKey),
}

/// A subscription on a connection of its own.
pub struct Subscription {
    connector: Connector,
    topics: Vec<Topic>,
    stream: Option<(Connection, u64)>,
    /// The first snapshot, until [`Self::next`] returns it.
    first: Option<Vec<Record>>,
}

impl Subscription {
    /// Subscribe, failing at once when the daemon refuses it.
    pub(crate) async fn open(connector: Connector, topics: Vec<Topic>) -> Result<Self, Error> {
        let (connection, id, records) = start(&connector, &topics).await?;
        Ok(Self {
            connector,
            topics,
            stream: Some((connection, id)),
            first: Some(records),
        })
    }

    /// The next update: first the snapshot, then each change, and a new
    /// snapshot after a reconnect.
    pub async fn next(&mut self) -> Result<Update, Error> {
        if let Some(records) = self.first.take() {
            return Ok(Update::Snapshot {
                records,
                resynced: false,
            });
        }
        loop {
            let Some((connection, id)) = &mut self.stream else {
                let records = self.reconnect().await?;
                return Ok(Update::Snapshot {
                    records,
                    resynced: true,
                });
            };
            let reply = match connection.reply(*id).await {
                Ok(reply) => reply,
                Err(Error::Closed | Error::Io(_)) => {
                    self.stream = None;
                    continue;
                }
                Err(error) => return Err(error),
            };
            match reply {
                Reply::Event(Event::Changed { record }) if record.key().is_some() => {
                    return Ok(Update::Changed(record));
                }
                Reply::Event(Event::Removed { key }) => return Ok(Update::Removed(key)),
                Reply::Event(Event::Changed { .. } | Event::Unknown) => {}
                Reply::Error(error) => return Err(Error::Api(error)),
                Reply::Event(Event::Snapshot { .. })
                | Reply::Page(_)
                | Reply::Result(_)
                | Reply::Progress(_) => {
                    return Err(Error::Protocol("unexpected reply to a subscription".into()));
                }
            }
        }
    }

    /// Subscribe again on a new connection, waiting for a restarting daemon.
    async fn reconnect(&mut self) -> Result<Vec<Record>, Error> {
        let deadline = tokio::time::Instant::now() + RECONNECT_TIMEOUT;
        let mut delay = RECONNECT_INITIAL;
        loop {
            match start(&self.connector, &self.topics).await {
                Ok((connection, id, records)) => {
                    self.stream = Some((connection, id));
                    return Ok(records);
                }
                Err(Error::Closed | Error::Io(_)) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(RECONNECT_MAX);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

/// Connect, subscribe and read the whole snapshot.
async fn start(
    connector: &Connector,
    topics: &[Topic],
) -> Result<(Connection, u64, Vec<Record>), Error> {
    let mut connection = connector.open().await?;
    let id = connection.send(subscribe_call(topics.to_vec())).await?;
    let mut records = Vec::new();
    loop {
        match connection.reply(id).await? {
            Reply::Event(Event::Snapshot {
                records: part,
                complete,
            }) => {
                records.extend(part.into_iter().filter(|record| record.key().is_some()));
                if complete {
                    return Ok((connection, id, records));
                }
            }
            Reply::Event(Event::Unknown) => {}
            Reply::Error(error) => return Err(Error::Api(error)),
            _ => {
                return Err(Error::Protocol(
                    "a subscription began without its snapshot".into(),
                ));
            }
        }
    }
}
