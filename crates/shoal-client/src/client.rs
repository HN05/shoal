//! Connecting to the daemon, the handshake, and calls.
use std::{path::PathBuf, time::Duration};

use tokio::{
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::Mutex,
};

use crate::{
    errors::{ApiError, Error, ErrorCode},
    methods::{Call, Method, Subscribe, Topic},
    subscription::Subscription,
    wire::{self, FrameReader, HandshakeReply, Hello, Opening, Reply, Request, Response, Welcome},
};

/// How long a call may take by default, as long as the daemon's slowest
/// operations, which run Git, hooks and simulators.
const CALL_TIMEOUT: Duration = Duration::from_secs(3600);
/// How long the handshake may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Where and as whom to connect.
#[derive(Debug, Clone)]
pub struct Connector {
    socket: PathBuf,
    scope: Option<String>,
    name: Option<String>,
    timeout: Duration,
}

impl Connector {
    /// Connect to the daemon listening on `socket`, as the user.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            scope: None,
            name: None,
            timeout: CALL_TIMEOUT,
        }
    }

    /// Connect to the current user's daemon, as [`crate::default_socket`] finds it.
    pub fn default_socket() -> std::io::Result<Self> {
        Ok(Self::new(crate::socket::default_socket()?))
    }

    /// Call as the agent of the workspace whose scope token this is.
    pub fn scope(mut self, token: impl Into<String>) -> Self {
        self.scope = Some(token.into());
        self
    }

    /// Name the client in the daemon's log.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// How long a call may take before it fails.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub async fn connect(self) -> Result<Client, Error> {
        let connection = self.open().await?;
        Ok(Client {
            welcome: connection.welcome.clone(),
            connector: self,
            connection: Mutex::new(Some(connection)),
        })
    }

    /// Subscribe to `topics` on a connection of its own.
    pub async fn subscribe(self, topics: Vec<Topic>) -> Result<Subscription, Error> {
        Subscription::open(self, topics).await
    }

    /// A new connection, past its handshake.
    pub(crate) async fn open(&self) -> Result<Connection, Error> {
        let stream = UnixStream::connect(&self.socket).await.map_err(Error::Io)?;
        let (reader, writer) = stream.into_split();
        let mut connection = Connection {
            reader: FrameReader::new(reader),
            writer,
            welcome: Welcome {
                version: String::new(),
                supported_majors: Vec::new(),
                features: Vec::new(),
            },
            next_id: 1,
        };
        let opening = Opening {
            client_api: Hello {
                version: crate::VERSION.into(),
                scope: self.scope.clone(),
                client: self.name.clone(),
            },
        };
        wire::write(&mut connection.writer, &opening).await?;
        let reply = tokio::time::timeout(HANDSHAKE_TIMEOUT, connection.read::<HandshakeReply>())
            .await
            .map_err(|_| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the daemon did not answer the handshake",
                ))
            })??;
        connection.welcome = match reply {
            HandshakeReply::Welcome(welcome) => welcome,
            HandshakeReply::Error(error) => return Err(Error::Api(error)),
        };
        Ok(connection)
    }

    pub(crate) fn call_timeout(&self) -> Duration {
        self.timeout
    }
}

/// One connection to the daemon, past its handshake.
pub(crate) struct Connection {
    reader: FrameReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    pub(crate) welcome: Welcome,
    next_id: u64,
}

impl Connection {
    /// Send `call`, returning its request ID.
    pub(crate) async fn send(&mut self, call: Call) -> Result<u64, Error> {
        let id = self.next_id;
        self.next_id += 1;
        wire::write(&mut self.writer, &Request { id, call }).await?;
        Ok(id)
    }

    /// The next frame; the daemon hanging up is [`Error::Closed`].
    pub(crate) async fn read<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, Error> {
        self.reader.read().await?.ok_or(Error::Closed)
    }

    /// The next reply to request `id`.
    pub(crate) async fn reply(&mut self, id: u64) -> Result<Reply, Error> {
        loop {
            let response: Response = self.read().await?;
            if response.id == id {
                return Ok(response.reply);
            }
        }
    }

    /// The result of request `id`, with the pages before it in front of its list.
    async fn result(&mut self, id: u64) -> Result<serde_json::Value, Error> {
        let mut pages = Vec::new();
        loop {
            match self.reply(id).await? {
                Reply::Page(items) => pages.extend(items),
                Reply::Result(result) if pages.is_empty() => return Ok(result),
                Reply::Result(serde_json::Value::Array(rest)) => {
                    pages.extend(rest);
                    return Ok(pages.into());
                }
                Reply::Result(_) => {
                    return Err(Error::Protocol(
                        "pages ended in a result that is not a list".into(),
                    ));
                }
                Reply::Error(error) => return Err(Error::Api(error)),
                Reply::Event(_) => return Err(Error::Protocol("an event answered a call".into())),
            }
        }
    }
}

/// A connection for calls, which reconnects on the next call after the
/// daemon closed it. Calls on one client run one at a time.
pub struct Client {
    connector: Connector,
    welcome: Welcome,
    connection: Mutex<Option<Connection>>,
}

impl Client {
    /// Connect to the daemon on `socket` as the user.
    pub async fn connect(socket: impl Into<PathBuf>) -> Result<Self, Error> {
        Connector::new(socket).connect().await
    }

    /// What the daemon serves, from the latest handshake.
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// Send `params` and wait for its result. A call cut off by the daemon
    /// closing the connection fails with [`Error::Closed`]; whether it took
    /// effect shows in the daemon's state.
    pub async fn call<M: Method>(&self, params: M) -> Result<M::Output, Error> {
        let params =
            serde_json::to_value(params).map_err(|error| Error::Protocol(error.to_string()))?;
        let call: Call = serde_json::from_value(serde_json::json!({
            "method": M::NAME,
            "params": params,
        }))
        .map_err(|error| Error::Protocol(error.to_string()))?;
        let result = self.call_value(call).await?;
        serde_json::from_value(result).map_err(|error| Error::Protocol(error.to_string()))
    }

    /// Subscribe to `topics` on a connection of its own.
    pub async fn subscribe(&self, topics: Vec<Topic>) -> Result<Subscription, Error> {
        self.connector.clone().subscribe(topics).await
    }

    /// Send `call` on the open connection, or a new one. The connection is
    /// kept only once the exchange ends cleanly: one that a cancelled or
    /// failed call left holding part of a request or reply is dropped.
    async fn call_value(&self, call: Call) -> Result<serde_json::Value, Error> {
        let mut kept = self.connection.lock().await;
        let mut open = match kept.take() {
            Some(open) => open,
            None => self.connector.open().await?,
        };
        let name = call.name();
        let exchange = async {
            let id = open.send(call).await?;
            open.result(id).await
        };
        let Ok(result) = tokio::time::timeout(self.connector.call_timeout(), exchange).await else {
            return Err(Error::Api(ApiError::new(
                ErrorCode::OperationFailed,
                format!("{name} did not finish in time"),
            )));
        };
        if !matches!(
            result,
            Err(Error::Closed | Error::Io(_) | Error::Protocol(_))
        ) {
            *kept = Some(open);
        }
        result
    }
}

/// `call` as the subscription request it is.
pub(crate) fn subscribe_call(topics: Vec<Topic>) -> Call {
    Call::Subscribe(Subscribe { topics })
}

#[cfg(test)]
mod tests;
