//! Following an operation: its progress, then its result or error.
use crate::{
    client::{Connection, Connector},
    errors::Error,
    methods::Call,
    operations::{FollowOperation, OperationEvent, OperationResult},
    wire::Reply,
};

/// What following an operation reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Followed {
    Progress(OperationEvent),
    /// The operation succeeded; nothing follows.
    Ended(OperationResult),
}

/// An operation followed on a connection of its own. The operation goes on
/// when the connection closes, as when the daemon restarts; a restart
/// forgets it, so following it again is refused with `not_found`.
pub struct Following {
    connection: Connection,
    id: u64,
}

impl Following {
    pub(crate) async fn open(
        connector: &Connector,
        operation: String,
        after: u64,
    ) -> Result<Self, Error> {
        let mut connection = connector.open().await?;
        let call = Call::FollowOperation(FollowOperation { operation, after });
        let id = connection.send(call).await?;
        Ok(Self { connection, id })
    }

    /// The next event, or the result once the operation succeeded. An
    /// operation that failed, or cannot be followed, fails with the
    /// daemon's typed error, such as `setup_failed` or `not_found`.
    pub async fn next(&mut self) -> Result<Followed, Error> {
        match self.connection.reply(self.id).await? {
            Reply::Progress(event) => Ok(Followed::Progress(event)),
            Reply::Result(result) => serde_json::from_value(result)
                .map(Followed::Ended)
                .map_err(|error| Error::Protocol(error.to_string())),
            Reply::Error(error) => Err(Error::Api(error)),
            Reply::Page(_) | Reply::Event(_) => Err(Error::Protocol(
                "unexpected reply to operation.follow".into(),
            )),
        }
    }
}
