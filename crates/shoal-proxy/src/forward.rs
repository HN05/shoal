//! Forwarding one agent request to the upstream and its response back.
use std::{convert::Infallible, sync::Arc, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, header, server::conn::http1, service::service_fn};
use hyper_rustls::HttpsConnector;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
};
use tokio::{net::TcpStream, sync::mpsc::UnboundedSender};

use crate::{
    Event, Upstream,
    clock::Clock,
    request::Buffered,
    response::{
        Body, bad_gateway, event_stream, full, is_event_stream, passthrough, prefixed,
        strip_hop_by_hop,
    },
    retry::{self, Bound, Failure, Outage, Policy},
    stream::{self, Opening, Protocol},
};

pub(crate) struct Forwarder {
    upstream: Upstream,
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    outage: Outage,
    clock: Arc<dyn Clock>,
    events: UnboundedSender<Event>,
}

/// How one attempt at a request ended.
enum Attempt {
    /// The upstream answered, and the agent gets the answer as it is.
    Answered(Response<Body>),
    Failed(Failed),
}

/// A failed attempt, with the answer the agent gets if it is the last.
struct Failed {
    failure: Failure,
    retry_after: Option<Duration>,
    /// Whether the failure arrived inside a response stream.
    in_stream: bool,
    /// The error object of an HTTP failure's JSON body, which the agent
    /// hears in a failure stream in place of the HTTP failure.
    error: Option<serde_json::Value>,
    response: Response<Body>,
}

impl Forwarder {
    pub(crate) fn new(
        upstream: Upstream,
        events: UnboundedSender<Event>,
        policy: Policy,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Self {
            upstream,
            client: Client::builder(TokioExecutor::new()).build(connector),
            outage: Outage::new(policy, clock.clone()),
            clock,
            events,
        }
    }

    /// Answer the agent's requests on `stream` until it closes. An agent
    /// that goes away mid-response only ends its connection.
    pub(crate) async fn serve_connection(self: Arc<Self>, stream: TcpStream) {
        let service = service_fn(move |request| {
            let forwarder = self.clone();
            async move { Ok::<_, Infallible>(forwarder.handle(request).await) }
        });
        let _ = http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .await;
    }

    async fn handle(&self, request: Request<Incoming>) -> Response<Body> {
        match Buffered::read(request).await {
            Ok(request) => self.forward(&request).await,
            Err(error) => bad_gateway(&error),
        }
    }

    /// Send `request` until the upstream answers it or the outage outlasts
    /// its budget, then give the agent the last answer.
    async fn forward(&self, request: &Buffered) -> Response<Body> {
        let mut attempt = 1;
        loop {
            let failed = match self.attempt(request, attempt > 1).await {
                Attempt::Answered(response) => {
                    self.answered(attempt);
                    return response;
                }
                Attempt::Failed(failed) => failed,
            };
            match self.wait_before_retry(&failed, attempt) {
                Some(wait) => self.clock.sleep(wait).await,
                None => return self.give_up(failed, request),
            }
            // Another request may have given up on the outage meanwhile.
            if self.outage.given_up() {
                return self.give_up(failed, request);
            }
            attempt += 1;
        }
    }

    /// Send `request` once. While the upstream is down the attempt gets
    /// only the outage's remaining budget, and a `retry` gets none once the
    /// outage is given up, so a stalled upstream cannot keep the proxy from
    /// giving up.
    async fn attempt(&self, request: &Buffered, retry: bool) -> Attempt {
        let remaining = match self.outage.bound() {
            Bound::Within(remaining) => remaining,
            Bound::GivenUp if retry => Duration::ZERO,
            // A recovered upstream takes a retry as any other request.
            Bound::Unbounded | Bound::GivenUp => return self.send(request).await,
        };
        if remaining.is_zero() {
            return Attempt::Failed(Failed {
                failure: Failure::Unreachable,
                retry_after: None,
                in_stream: false,
                error: None,
                response: bad_gateway(&anyhow::anyhow!(
                    "the proxy gave up on the upstream's outage"
                )),
            });
        }
        tokio::select! {
            attempt = self.send(request) => attempt,
            () = self.clock.deadline(remaining) => Attempt::Failed(Failed {
                failure: Failure::Unreachable,
                retry_after: None,
                in_stream: false,
                error: None,
                response: bad_gateway(&anyhow::anyhow!(
                    "the upstream did not answer before the proxy gave up on its outage"
                )),
            }),
        }
    }

    async fn send(&self, request: &Buffered) -> Attempt {
        let sent = match request.to_upstream(&self.upstream) {
            Ok(sent) => self.client.request(sent).await,
            Err(error) => return Attempt::Answered(bad_gateway(&error)),
        };
        let response = match sent {
            Ok(response) => response,
            Err(error) => {
                let error = anyhow::Error::from(error).context("reach the upstream");
                return Attempt::Failed(Failed {
                    failure: Failure::Unreachable,
                    retry_after: None,
                    in_stream: false,
                    error: None,
                    response: bad_gateway(&error),
                });
            }
        };
        match (Failure::of_status(response.status()), request.protocol) {
            (Some(failure), _) => failed_status(failure, response, self.clock.as_ref()).await,
            (None, Some(protocol)) if is_event_stream(&response) => {
                opened(protocol, response).await
            }
            (None, _) => Attempt::Answered(passthrough(response)),
        }
    }

    /// An answer ends the outage; one that took retries is a recovery.
    fn answered(&self, attempt: u32) {
        self.outage.ended();
        if attempt > 1 {
            self.report(Event::Recovered);
        }
    }

    /// The wait before trying again after `failed`, reported; `None` when
    /// the agent gets the failure.
    fn wait_before_retry(&self, failed: &Failed, attempt: u32) -> Option<Duration> {
        if !failed.failure.retryable() {
            return None;
        }
        let wait = self.outage.next_wait(attempt, failed.retry_after)?;
        self.report(Event::Retrying {
            failure: failed.failure,
            attempt,
            wait,
        });
        Some(wait)
    }

    /// The answer the agent gets for the last `failed` attempt, reported
    /// when the proxy retries such failures. A streaming request whose
    /// protocol ends a turn on a failure inside the stream gets one in place
    /// of an HTTP failure, which the agent would retry itself.
    fn give_up(&self, failed: Failed, request: &Buffered) -> Response<Body> {
        if !failed.failure.retryable() {
            return failed.response;
        }
        let failed = match request.failure_stream(failed.failure, failed.error.as_ref()) {
            Some(stream) if !failed.in_stream => Failed {
                in_stream: true,
                response: event_stream(stream),
                ..failed
            },
            _ => failed,
        };
        self.report(Event::Failed {
            failure: failed.failure,
            in_stream: failed.in_stream,
        });
        failed.response
    }

    /// Tell the proxy's owner, which may have stopped listening.
    fn report(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

/// The most of an HTTP failure's body the proxy reads.
const ERROR_BODY_LIMIT: usize = 1 << 20;
/// How long the proxy waits for an HTTP failure's body.
const ERROR_BODY_WAIT: Duration = Duration::from_secs(30);

/// An HTTP failure's body, or none when it is larger than an error needs or
/// does not arrive in time, which leaves the failure as its status says.
async fn error_body(body: Incoming, clock: &dyn Clock) -> Bytes {
    let read = http_body_util::Limited::new(body, ERROR_BODY_LIMIT).collect();
    tokio::select! {
        read = read => read.map(|body| body.to_bytes()).unwrap_or_default(),
        () = clock.deadline(ERROR_BODY_WAIT) => Bytes::new(),
    }
}

/// An HTTP failure, read whole since it is small: its error may say when
/// to try again, and is what the agent hears if the proxy gives up.
async fn failed_status(
    failure: Failure,
    response: Response<Incoming>,
    clock: &dyn Clock,
) -> Attempt {
    let (mut parts, body) = response.into_parts();
    strip_hop_by_hop(&mut parts.headers);
    parts.headers.remove(header::CONTENT_LENGTH);
    let body = error_body(body, clock).await;
    let error = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .map(|body| body["error"].clone())
        .filter(serde_json::Value::is_object);
    let retry_after =
        retry::retry_after(&parts.headers).or_else(|| error.as_ref().and_then(retry::error_wait));
    Attempt::Failed(Failed {
        failure,
        retry_after,
        in_stream: false,
        error,
        response: Response::from_parts(parts, full(body)),
    })
}

/// The first events of `response`'s stream: a failure among them, or an end
/// before any content, is an attempt that failed; content goes to the agent.
async fn opened(protocol: Protocol, response: Response<Incoming>) -> Attempt {
    let (mut parts, mut body) = response.into_parts();
    strip_hop_by_hop(&mut parts.headers);
    parts.headers.remove(header::CONTENT_LENGTH);
    match stream::open(protocol, &mut body).await {
        Ok(Opening::Content(read)) => {
            Attempt::Answered(Response::from_parts(parts, prefixed(read, body)))
        }
        // An agent that gets it must not take it for a whole answer.
        Ok(Opening::Ended(_)) => Attempt::Failed(Failed {
            failure: Failure::ServerError,
            retry_after: None,
            in_stream: false,
            error: None,
            response: bad_gateway(&anyhow::anyhow!(
                "the upstream's stream ended before any content"
            )),
        }),
        // A usage limit's reset in the event says how long to wait, as in
        // an HTTP failure's body.
        Ok(Opening::Failure(failure, error, read)) => Attempt::Failed(Failed {
            failure,
            retry_after: error.as_ref().and_then(retry::error_wait),
            in_stream: true,
            error,
            response: Response::from_parts(parts, full(read)),
        }),
        Err(error) => Attempt::Failed(Failed {
            failure: Failure::ServerError,
            retry_after: None,
            in_stream: false,
            error: None,
            response: bad_gateway(&error),
        }),
    }
}

#[cfg(test)]
mod tests;
