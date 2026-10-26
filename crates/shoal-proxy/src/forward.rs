//! Forwarding one agent request to the upstream and its response back.
use std::{convert::Infallible, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode,
    body::Incoming,
    header::{self, HeaderName, HeaderValue},
    server::conn::http1,
    service::service_fn,
};
use hyper_rustls::HttpsConnector;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
};
use tokio::{net::TcpStream, sync::mpsc::UnboundedSender};

use crate::{
    Event, Upstream,
    retry::{self, Failure, Outage},
};

pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;
/// What the proxy answers the agent with.
pub(crate) type Body = UnsyncBoxBody<Bytes, BoxError>;

/// Headers that describe one connection, not the request, so they are
/// never forwarded.
const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
];

pub(crate) struct Forwarder {
    upstream: Upstream,
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    outage: Outage,
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
    response: Response<Body>,
}

impl Forwarder {
    pub(crate) fn new(upstream: Upstream, events: UnboundedSender<Event>) -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Self {
            upstream,
            client: Client::builder(TokioExecutor::new()).build(connector),
            outage: Outage::default(),
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
            let failed = match self.attempt(request).await {
                Attempt::Answered(response) => {
                    self.answered(attempt);
                    return response;
                }
                Attempt::Failed(failed) => failed,
            };
            match self.wait_before_retry(&failed, attempt) {
                Some(wait) => tokio::time::sleep(wait).await,
                None => return failed.response,
            }
            attempt += 1;
        }
    }

    async fn attempt(&self, request: &Buffered) -> Attempt {
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
                    response: bad_gateway(&error),
                });
            }
        };
        match Failure::of_status(response.status()) {
            None => Attempt::Answered(passthrough(response)),
            Some(failure) => Attempt::Failed(Failed {
                failure,
                retry_after: retry::retry_after(response.headers()),
                response: passthrough(response),
            }),
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
    /// the agent gets the failure, which is reported when the proxy gave up
    /// on it.
    fn wait_before_retry(&self, failed: &Failed, attempt: u32) -> Option<Duration> {
        if !failed.failure.retryable() {
            return None;
        }
        let Some(wait) = self.outage.next_wait(attempt, failed.retry_after) else {
            self.report(Event::Failed {
                failure: failed.failure,
                in_stream: false,
            });
            return None;
        };
        self.report(Event::Retrying {
            failure: failed.failure,
            attempt,
            wait,
        });
        Some(wait)
    }

    /// Tell the proxy's owner, which may have stopped listening.
    fn report(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

/// An agent request read whole, so the proxy can send it again.
pub(crate) struct Buffered {
    method: Method,
    path_and_query: String,
    headers: HeaderMap,
    pub(crate) body: Bytes,
}

impl Buffered {
    pub(crate) async fn read(request: Request<Incoming>) -> Result<Self> {
        let (parts, body) = request.into_parts();
        let body = body
            .collect()
            .await
            .context("read the agent's request")?
            .to_bytes();
        Ok(Self {
            method: parts.method,
            path_and_query: parts
                .uri
                .path_and_query()
                .map_or("/", |path| path.as_str())
                .to_owned(),
            headers: parts.headers,
            body,
        })
    }

    /// This request addressed to `upstream`, with its own headers but the
    /// upstream's `Host`.
    pub(crate) fn to_upstream(&self, upstream: &Upstream) -> Result<Request<Full<Bytes>>> {
        let mut request = Request::builder()
            .method(self.method.clone())
            .uri(upstream.join(&self.path_and_query)?)
            .body(Full::new(self.body.clone()))?;
        let headers = request.headers_mut();
        for (name, value) in &self.headers {
            if forwarded(name) && name != header::HOST && name != header::CONTENT_LENGTH {
                headers.append(name, value.clone());
            }
        }
        headers.insert(header::HOST, HeaderValue::from_str(upstream.authority())?);
        Ok(request)
    }
}

fn forwarded(name: &HeaderName) -> bool {
    !HOP_BY_HOP.contains(&name.as_str())
}

/// The upstream's response as the agent receives it, streamed as it comes.
pub(crate) fn passthrough(response: Response<Incoming>) -> Response<Body> {
    let (parts, body) = response.into_parts();
    let mut response = Response::from_parts(parts, body.map_err(BoxError::from).boxed_unsync());
    strip_hop_by_hop(response.headers_mut());
    response
}

pub(crate) fn strip_hop_by_hop(headers: &mut HeaderMap) {
    for name in HOP_BY_HOP {
        headers.remove(name);
    }
}

/// The answer when the proxy could not reach the upstream at all.
fn bad_gateway(error: &anyhow::Error) -> Response<Body> {
    let mut response = Response::new(
        Full::new(Bytes::from(format!("shoal-proxy: {error:#}\n")))
            .map_err(BoxError::from)
            .boxed_unsync(),
    );
    *response.status_mut() = StatusCode::BAD_GATEWAY;
    response
}
