//! The hub's listener: HTTP on `[machine.hub] listen`, which upgrades a
//! request to the link's path to a WebSocket carrying a member's link. It
//! handles no TLS, which a proxy or tunnel in front of it provides, and
//! binds again once a reload changes its address or the hub role.
use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result};
use http_body_util::Empty;
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
    header::{self, HeaderValue},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::Value;
use shoal_protocol::hub::{Listener, link::PATH};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{handshake::derive_accept_key, protocol::Role},
};

use super::{accept::Over, transport::Channel};
use crate::daemon::{log, notifications::NotificationKind, workspace::Manager};

/// How long a client may take to send a request's headers, and then the
/// request that opens its link.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The most connections the listener serves at once; more wait.
const MAX_CONNECTIONS: usize = 256;

/// Where the listener is bound, or why it is not, none while the daemon is
/// not the hub.
#[derive(Default)]
pub(super) struct Bound(Mutex<Option<Result<SocketAddr, String>>>);

/// Serve the listener while this daemon is the hub, until the daemon stops.
pub(super) async fn run(manager: Arc<Manager>) {
    let mut reloaded = manager.config_state.watch();
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut listening: Option<(SocketAddr, TcpListener)> = None;
    loop {
        let wanted = manager
            .hub_settings()
            .filter(|hub| hub.enabled)
            .map(|hub| hub.listen);
        if listening.as_ref().map(|(address, _)| *address) != wanted || listening.is_none() {
            listening = None;
            if let Some(address) = wanted {
                listening = manager
                    .bind_hub(address)
                    .await
                    .map(|listener| (address, listener));
            } else {
                *manager.hub.listener.0.lock().unwrap() = None;
            }
        }
        let listener = listening.as_ref().map(|(_, listener)| listener);
        tokio::select! {
            _ = reloaded.changed() => {}
            () = accept(&manager, &connections, listener) => {}
        }
        reloaded.borrow_and_update();
    }
}

impl Manager {
    /// A listener on `address`, none when it cannot bind, which the user
    /// hears of.
    async fn bind_hub(&self, address: SocketAddr) -> Option<TcpListener> {
        let bound = TcpListener::bind(address)
            .await
            .and_then(|listener| Ok((listener.local_addr()?, listener)));
        let (state, listener) = match bound {
            Ok((local, listener)) => {
                log!("hub: listening on {local}");
                (Ok(local), Some(listener))
            }
            Err(error) => {
                let message = format!(
                    "Shoal could not listen for the hub's members on {address}: {error}; change [machine.hub] listen and run shoal config reload"
                );
                log!("hub: {message}");
                (self.notify(None, NotificationKind::EnvironmentProblem, &message)).await;
                (Err(message), None)
            }
        };
        *self.hub.listener.0.lock().unwrap() = Some(state);
        listener
    }

    /// The hub's listener as members see it: the address it is bound to and
    /// the URL it advertises.
    pub(crate) fn hub_listener(&self) -> Listener {
        let settings = self.hub_settings().unwrap_or_default();
        let bound = self.hub.listener.0.lock().unwrap().clone();
        let (address, error) = match bound {
            Some(Ok(address)) => (Some(address), None),
            Some(Err(error)) => (None, Some(error)),
            None => (None, Some("the hub's listener has not started".into())),
        };
        Listener {
            address,
            url: settings.advertised(address.unwrap_or(settings.listen)),
            error,
        }
    }

    /// Take the link a member opens on `channel` with its first message.
    async fn open_listener_link(self: &Arc<Self>, mut channel: Channel) {
        let first = tokio::time::timeout(READ_TIMEOUT, channel.expect::<Value>()).await;
        let opening = match first {
            Ok(Ok(first)) => opening(first),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(anyhow::anyhow!(
                "no opening within {}s",
                READ_TIMEOUT.as_secs()
            )),
        };
        match opening {
            Ok((protocol, hello)) => {
                self.accept_member(channel, protocol, hello, Over::Listener)
                    .await
            }
            Err(error) => log!("hub: a listener connection opened no link: {error:#}"),
        }
    }
}

/// The protocol and hello of the request that opens a link, read without
/// the rest of its shape, which a member of another release may change.
fn opening(mut request: Value) -> Result<(u32, Value)> {
    let protocol = (request["protocol"].as_u64())
        .and_then(|protocol| u32::try_from(protocol).ok())
        .context("the opening request names no protocol")?;
    let hello = (request.get_mut("method"))
        .and_then(|method| method.get_mut("hub_link"))
        .map(Value::take)
        .filter(|hello| !hello.is_null())
        .context("the opening request opens no hub link")?;
    Ok((protocol, hello))
}

/// Serve each connection to `listener`, holding one of the `connections`
/// permits each; none never accepts.
async fn accept(
    manager: &Arc<Manager>,
    connections: &Arc<Semaphore>,
    listener: Option<&TcpListener>,
) {
    let Some(listener) = listener else {
        return std::future::pending().await;
    };
    loop {
        let permit = connections
            .clone()
            .acquire_owned()
            .await
            .expect("never closed");
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(serve(manager.clone(), stream, Arc::new(permit)));
            }
            Err(error) => {
                log!("hub: accept failed: {error}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Answer the requests on `stream` until it closes or upgrades, holding
/// `permit` until then and through the link an upgrade carries.
async fn serve(manager: Arc<Manager>, stream: TcpStream, permit: Arc<OwnedSemaphorePermit>) {
    let service = service_fn(move |request| {
        let (manager, permit) = (manager.clone(), permit.clone());
        async move { Ok::<_, Infallible>(respond(manager, request, permit)) }
    });
    let _ = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(READ_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await;
}

/// The response to `request`: a WebSocket upgrade at the link's path, whose
/// link a task then serves, and `404` for anything else.
fn respond(
    manager: Arc<Manager>,
    request: Request<Incoming>,
    permit: Arc<OwnedSemaphorePermit>,
) -> Response<Empty<Bytes>> {
    let key = request.headers().get(header::SEC_WEBSOCKET_KEY).cloned();
    let upgrades = request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|upgrade| upgrade.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let (Some(key), true, PATH) = (key, upgrades, request.uri().path()) else {
        return status(StatusCode::NOT_FOUND);
    };
    tokio::spawn(async move {
        match hyper::upgrade::on(request).await {
            Ok(upgraded) => {
                let socket = TokioIo::new(upgraded);
                let socket = WebSocketStream::from_raw_socket(socket, Role::Server, None).await;
                manager.open_listener_link(Channel::websocket(socket)).await;
                drop(permit);
            }
            Err(error) => log!("hub: a WebSocket upgrade failed: {error}"),
        }
    });
    let mut response = status(StatusCode::SWITCHING_PROTOCOLS);
    let headers = response.headers_mut();
    headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    let accept = derive_accept_key(key.as_bytes());
    headers.insert(
        header::SEC_WEBSOCKET_ACCEPT,
        HeaderValue::from_str(&accept).expect("base64 is a valid header value"),
    );
    response
}

/// A response with `status` and no body.
fn status(status: StatusCode) -> Response<Empty<Bytes>> {
    let mut response = Response::new(Empty::new());
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests;
