//! The client API that `crates/shoal-client` defines: a stable protocol for
//! apps other than the CLI, served on the daemon's socket beside the
//! internal protocol. A connection handshakes once, then sends requests one
//! at a time, or subscribes. Its requests run the daemon's own operations
//! under the internal protocol's scope rules and connection limits.
mod agents;
mod calls;
mod errors;
mod handshake;
mod records;
mod snapshot;
mod subscription;

use anyhow::Result;
use shoal_client::{
    ApiError, Call, ErrorCode,
    wire::{self, FrameReader, HandshakeReply, Reply, Request, Response},
};
use tokio::net::{
    UnixStream,
    unix::{OwnedReadHalf, OwnedWriteHalf},
};

use super::{Server, scope::Caller};

/// A connection past its handshake.
pub(super) struct Session {
    server: Server,
    /// The caller's scope token, which authorizes each request as the
    /// internal protocol's do.
    scope: Option<String>,
    caller: Option<Caller>,
}

type Reader = FrameReader<OwnedReadHalf>;

/// Serve a client API connection, whose first frame held `hello`: its
/// handshake, then its requests until it hangs up or subscribes.
pub(super) async fn serve(
    stream: UnixStream,
    server: Server,
    hello: serde_json::Value,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = FrameReader::new(reader);
    let (_slot, session) = match open(&server, hello).await {
        Ok(opened) => opened,
        Err(error) => {
            return wire::write(&mut writer, &HandshakeReply::Error(error))
                .await
                .map_err(Into::into);
        }
    };
    wire::write(&mut writer, &handshake::welcome()).await?;
    while let Some(frame) = reader.read::<serde_json::Value>().await? {
        let (id, call) = match request(frame) {
            Ok(request) => (request.id, request.call),
            Err((id, error)) => {
                respond(&mut writer, id, Err(error)).await?;
                continue;
            }
        };
        if let Call::Subscribe(subscribe) = call {
            return subscription::run(session, id, subscribe, reader, writer).await;
        }
        session.answer(&mut writer, id, call).await?;
    }
    Ok(())
}

/// The session `hello` opens, holding long-lived capacity while it lasts.
async fn open(
    server: &Server,
    hello: serde_json::Value,
) -> Result<(super::connections::Slot, Session), ApiError> {
    let slot = server
        .capacity
        .long_lived()
        .map_err(|error| ApiError::new(ErrorCode::Busy, format!("{error:#}")))?;
    Ok((slot, handshake::accept(server, hello).await?))
}

/// The request `frame` holds, or its ID with why it cannot be read.
fn request(frame: serde_json::Value) -> Result<Request, (u64, ApiError)> {
    let id = frame["id"].as_u64().unwrap_or_default();
    let method = frame["method"].as_str().unwrap_or_default().to_owned();
    serde_json::from_value(frame).map_err(|error| {
        let error = match Call::NAMES.contains(&method.as_str()) {
            true => errors::invalid(format!("invalid {method} request: {error}")),
            false => errors::invalid(format!("this daemon does not serve the method {method:?}")),
        };
        (id, error)
    })
}

impl Session {
    /// Answer request `id`, `call`, within a request slot, holding an
    /// operation guard until its reply is written, so a daemon update
    /// cannot cut the reply off after the operation took effect.
    async fn answer(&self, writer: &mut OwnedWriteHalf, id: u64, call: Call) -> Result<()> {
        let slot = self
            .server
            .capacity
            .request(format!("client API {}", call.name()))
            .map_err(|error| ApiError::new(ErrorCode::Busy, format!("{error:#}")));
        let _slot = match slot {
            Ok(slot) => slot,
            Err(error) => return respond(writer, id, Err(error)).await,
        };
        let _operation = self.server.manager.gates.owned_operation().await;
        let result = calls::answer(self, call).await;
        respond(writer, id, result).await
    }
}

/// Send `result` to request `id`: a long list in pages, then the rest.
async fn respond(
    writer: &mut OwnedWriteHalf,
    id: u64,
    result: Result<serde_json::Value, ApiError>,
) -> Result<()> {
    let replies: Vec<Reply> = match result {
        Ok(serde_json::Value::Array(items)) => {
            let mut pages = wire::pages(items)?;
            let last = pages.pop().unwrap_or_default();
            pages
                .into_iter()
                .map(Reply::Page)
                .chain([Reply::Result(last.into())])
                .collect()
        }
        Ok(value) => vec![Reply::Result(value)],
        Err(error) => vec![Reply::Error(error)],
    };
    for reply in replies {
        wire::write(writer, &Response { id, reply }).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
