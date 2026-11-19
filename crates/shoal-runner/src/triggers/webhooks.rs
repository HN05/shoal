//! The webhook listener: the HTTP server `[machine.webhooks] listen` sets
//! up for webhook triggers, with no routes but theirs. It binds again once
//! a reload changes the address, and reads each request whole, within a
//! size limit, before its delivery decides what becomes of it.
use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use http_body_util::{BodyExt, Empty, Limited};
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use shoal_daemon_base::notifications::About;
use shoal_protocol::{alerts::AlertKind, triggers::WebhookRequest};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};

use crate::{Runner, daemon::Daemon, log};

mod delivery;

use delivery::Delivered;

/// The largest body a request may send.
pub(crate) const MAX_BODY: usize = 5 * 1024 * 1024;

/// The most requests read, checked or waiting to fire at once; one more
/// gets `503`, so that requests cannot take the daemon's memory or start
/// unbounded secret commands.
pub(crate) const MAX_DELIVERIES: usize = 16;

/// How long a client may take to send a request's headers, and then its
/// body.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The listener for an address, or the failure to bind it, which waits
/// for a reload.
struct Bound {
    address: Option<SocketAddr>,
    listener: Option<TcpListener>,
}

/// Serve webhook requests on the configured address, until the daemon
/// stops.
pub async fn run(runner: Arc<Runner>, daemon: Arc<dyn Daemon>) {
    let mut reloaded = runner.config.watch();
    let deliveries = Arc::new(Semaphore::new(MAX_DELIVERIES));
    let mut bound = Bound {
        address: None,
        listener: None,
    };
    loop {
        let address = runner.webhook_address(&*daemon);
        // A reload may also fix what kept the address from binding.
        if bound.address != address || bound.listener.is_none() {
            drop(bound.listener.take());
            bound = Bound {
                address,
                listener: runner.bind_webhooks(address).await,
            };
        }
        tokio::select! {
            _ = reloaded.changed() => {}
            () = accept(&runner, &daemon, &deliveries, bound.listener.as_ref()) => {}
        }
        reloaded.borrow_and_update();
    }
}

impl Runner {
    /// The address `[machine.webhooks] listen` sets, none without it or in
    /// a daemon that leaves the user's config to the installed Shoal.
    pub(super) fn webhook_address(&self, daemon: &dyn Daemon) -> Option<SocketAddr> {
        let address = self.config().machine.webhooks.listen;
        address.filter(|_| daemon.acts_on_global_config())
    }

    /// A listener on `address`, none without one or when it cannot bind,
    /// which the user hears of.
    pub(super) async fn bind_webhooks(&self, address: Option<SocketAddr>) -> Option<TcpListener> {
        let address = address?;
        match TcpListener::bind(address).await {
            Ok(listener) => Some(listener),
            Err(error) => {
                let message = format!(
                    "Shoal could not listen for webhooks on {address}: {error}; change [machine.webhooks] listen and run shoal config reload"
                );
                log!("webhooks: {message}");
                let kind = AlertKind::EnvironmentProblem;
                (self.notifications)
                    .notify_user(About::default(), kind, message)
                    .await;
                None
            }
        }
    }
}

/// Answer each connection to `listener` on its own, with at most the
/// `deliveries` permits in flight; none never accepts.
async fn accept(
    runner: &Arc<Runner>,
    daemon: &Arc<dyn Daemon>,
    deliveries: &Arc<Semaphore>,
    listener: Option<&TcpListener>,
) {
    let Some(listener) = listener else {
        return std::future::pending().await;
    };
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let (runner, daemon) = (runner.clone(), daemon.clone());
                tokio::spawn(serve(runner, daemon, deliveries.clone(), stream));
            }
            Err(error) => {
                log!("webhooks: accept failed: {error}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Answer the requests on `stream` until it closes.
async fn serve(
    runner: Arc<Runner>,
    daemon: Arc<dyn Daemon>,
    deliveries: Arc<Semaphore>,
    stream: TcpStream,
) {
    let service = service_fn(move |request| {
        let (runner, daemon) = (runner.clone(), daemon.clone());
        let deliveries = deliveries.clone();
        async move { Ok::<_, Infallible>(respond(&runner, &daemon, deliveries, request).await) }
    });
    let _ = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(READ_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service)
        .await;
}

/// The empty response `request` gets: `202` once accepted, before its
/// firing runs, so what the filter and key decide never shows; `404` for a
/// request no trigger takes, `413` for a body over [`MAX_BODY`] and `503`
/// while every one of the `deliveries` permits is taken.
async fn respond(
    runner: &Arc<Runner>,
    daemon: &Arc<dyn Daemon>,
    deliveries: Arc<Semaphore>,
    request: Request<Incoming>,
) -> Response<Empty<Bytes>> {
    let Ok(permit) = deliveries.try_acquire_owned() else {
        return empty(StatusCode::SERVICE_UNAVAILABLE);
    };
    let status = match read(request).await {
        Read::NotFound => StatusCode::NOT_FOUND,
        Read::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        Read::Failed(error) => {
            log!("webhooks: could not read a request: {error:#}");
            StatusCode::BAD_REQUEST
        }
        Read::Request {
            path,
            request,
            body,
        } => match (runner.deliver_webhook(daemon, &path, request, &body, permit)).await {
            Ok(Delivered::Accepted) => StatusCode::ACCEPTED,
            Ok(Delivered::Refused) => StatusCode::NOT_FOUND,
            Err(error) => {
                log!("webhooks: {path}: {error:#}");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        },
    };
    empty(status)
}

/// A response with `status` and no body.
fn empty(status: StatusCode) -> Response<Empty<Bytes>> {
    let mut response = Response::new(Empty::new());
    *response.status_mut() = status;
    response
}

/// What reading a request found.
enum Read {
    /// A POST with its path, the request as its trigger's input reads it,
    /// and its body as sent, which a signature covers.
    Request {
        path: String,
        request: WebhookRequest,
        body: Bytes,
    },
    /// Another method, which no route takes.
    NotFound,
    TooLarge,
    Failed(anyhow::Error),
}

/// Read `request` whole.
async fn read(request: Request<Incoming>) -> Read {
    if request.method() != Method::POST {
        return Read::NotFound;
    }
    let (parts, body) = request.into_parts();
    let collected = tokio::time::timeout(READ_TIMEOUT, Limited::new(body, MAX_BODY).collect());
    let body = match collected.await {
        Ok(Ok(body)) => body.to_bytes(),
        Ok(Err(error)) if error.is::<http_body_util::LengthLimitError>() => return Read::TooLarge,
        Ok(Err(error)) => return Read::Failed(anyhow::anyhow!(error)),
        Err(elapsed) => return Read::Failed(anyhow::Error::new(elapsed).context("read the body")),
    };
    let headers = match headers(&parts.headers) {
        Ok(headers) => headers,
        Err(error) => return Read::Failed(error),
    };
    let query = (parts.uri.query())
        .map(|query| {
            form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default();
    Read::Request {
        path: parts.uri.path().to_owned(),
        request: WebhookRequest {
            headers,
            query,
            body: String::from_utf8_lossy(&body).into_owned(),
        },
        body,
    }
}

/// Each header with its value, refusing a value that is not text.
fn headers(headers: &HeaderMap) -> Result<Vec<(String, String)>> {
    (headers.iter())
        .map(|(name, value)| {
            let value = value.to_str().with_context(|| format!("header {name}"))?;
            Ok((name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests;
