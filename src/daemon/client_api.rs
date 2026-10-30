//! The client API that `crates/shoal-client` defines: a stable protocol for
//! apps other than the CLI, served on the daemon's socket beside the
//! internal protocol. A connection handshakes once, then sends requests one
//! at a time, or subscribes or follows an operation. Its requests run the daemon's own operations
//! under the internal protocol's scope rules and connection limits.
mod agents;
mod calls;
mod errors;
mod following;
mod handshake;
mod progress;
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

use super::{Server, scope::Caller, workspace::Manager};

/// A connection past its handshake.
pub(super) struct Session {
    server: Server,
    /// The caller's scope token, which authorizes each request as the
    /// internal protocol's do.
    scope: Option<String>,
    caller: Option<Caller>,
}

type Reader = FrameReader<OwnedReadHalf>;

/// [`serve_connection`] on the heap, so serving a client API connection
/// adds nothing to the stack of the internal protocol's requests, which
/// share the function that reads a connection's first frame.
pub(super) fn serve(
    stream: UnixStream,
    server: Server,
    hello: serde_json::Value,
) -> futures_util::future::BoxFuture<'static, Result<()>> {
    Box::pin(serve_connection(stream, server, hello))
}

/// Serve a client API connection, whose first frame held `hello`: its
/// handshake, then its requests until it hangs up or subscribes.
async fn serve_connection(
    stream: UnixStream,
    server: Server,
    hello: serde_json::Value,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = FrameReader::new(reader);
    let manager = server.manager.clone();
    let (_slot, session) = match open(&server, hello).await {
        Ok(opened) => opened,
        Err(error) => {
            write_guarded(&manager, &mut writer, &HandshakeReply::Error(error)).await?;
            return Ok(());
        }
    };
    if !write_guarded(&manager, &mut writer, &handshake::welcome()).await? {
        return Ok(());
    }
    while let Some(frame) = reader.read::<serde_json::Value>().await? {
        let (id, call) = match request(frame) {
            Ok(request) => (request.id, request.call),
            Err((id, error)) => match refuse(&manager, &mut writer, id, error).await? {
                true => continue,
                false => return Ok(()),
            },
        };
        match call {
            Call::Subscribe(subscribe) => {
                return subscription::run(session, id, subscribe, reader, writer).await;
            }
            Call::FollowOperation(follow) => {
                return following::run(session, id, follow, reader, writer).await;
            }
            call => {
                if !session.answer(&mut writer, id, call).await? {
                    return Ok(());
                }
            }
        }
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
    /// The caller the scope token names now, refusing a token revoked or
    /// expired since the handshake.
    async fn current_caller(&self) -> Result<Option<Caller>, ApiError> {
        let Some(token) = &self.scope else {
            return Ok(None);
        };
        self.server
            .manager
            .caller(token)
            .await
            .map_err(|error| errors::failed(&error))?
            .map(Some)
            .ok_or_else(|| {
                ApiError::new(ErrorCode::ScopeDenied, "expired or unknown workspace scope")
            })
    }

    /// Answer request `id`, `call`, within a request slot, holding an
    /// operation guard until its reply is written, so a daemon update
    /// cannot cut the reply off after the operation took effect. `false`
    /// when the daemon quiesces for an update before the call can run, so
    /// the connection closes unanswered.
    async fn answer(&self, writer: &mut OwnedWriteHalf, id: u64, call: Call) -> Result<bool> {
        let manager = &self.server.manager;
        let slot = self
            .server
            .capacity
            .request(format!("client API {}", call.name()))
            .map_err(|error| ApiError::new(ErrorCode::Busy, format!("{error:#}")));
        let _slot = match slot {
            Ok(slot) => slot,
            Err(error) => return refuse(manager, writer, id, error).await,
        };
        let _operation = manager.gates.owned_operation().await;
        let result = calls::answer(self, call).await;
        respond(writer, id, result).await?;
        Ok(true)
    }
}

/// Write `value` under a short operation guard, or leave it unwritten while
/// a daemon update quiesces operations, returning whether it was written;
/// the caller then closes the connection, and the client tries again on the
/// replacement daemon.
pub(super) async fn write_guarded(
    manager: &Manager,
    writer: &mut OwnedWriteHalf,
    value: &impl serde::Serialize,
) -> Result<bool> {
    let Some(_operation) = manager.gates.try_operation() else {
        return Ok(false);
    };
    wire::write(writer, value).await?;
    Ok(true)
}

/// Refuse request `id` with `error`, as [`write_guarded`] writes.
async fn refuse(
    manager: &Manager,
    writer: &mut OwnedWriteHalf,
    id: u64,
    error: ApiError,
) -> Result<bool> {
    let reply = Reply::Error(error);
    write_guarded(manager, writer, &Response { id, reply }).await
}

/// Wait until the client hangs up. It sends nothing on a stream, so
/// anything readable means it closed the connection.
async fn hangup(reader: &mut Reader) {
    let mut closed = [0u8; 1];
    let _ = tokio::io::AsyncReadExt::read(reader.get_mut(), &mut closed).await;
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
