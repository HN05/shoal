//! The proxy against a fake upstream on a loopback port.
use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Request, Response, StatusCode, body::Incoming, server::conn::http1,
    service::service_fn,
};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
};
use tokio::{net::TcpListener, sync::mpsc::UnboundedReceiver};

use super::*;

/// A request as the upstream received it.
#[derive(Debug, Clone)]
pub(crate) struct Received {
    pub path: String,
    pub headers: HeaderMap,
    pub body: String,
}

/// An upstream that answers each request with what `reply` returns for the
/// requests it has received so far, the current one last.
pub(crate) struct FakeUpstream {
    pub addr: SocketAddr,
    pub received: Arc<Mutex<Vec<Received>>>,
}

impl FakeUpstream {
    pub(crate) async fn start(
        reply: impl Fn(&[Received]) -> Response<Full<Bytes>> + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let reply = Arc::new(reply);
        let log = received.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (reply, log) = (reply.clone(), log.clone());
                let service = service_fn(move |request: Request<Incoming>| {
                    let (reply, log) = (reply.clone(), log.clone());
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = body.collect().await.unwrap().to_bytes();
                        let mut log = log.lock().unwrap();
                        log.push(Received {
                            path: parts.uri.to_string(),
                            headers: parts.headers,
                            body: String::from_utf8_lossy(&body).into_owned(),
                        });
                        Ok::<_, Infallible>(reply(&log))
                    }
                });
                tokio::spawn(http1::Builder::new().serve_connection(TokioIo::new(stream), service));
            }
        });
        Self { addr, received }
    }

    pub(crate) fn url(&self, prefix: &str) -> Upstream {
        Upstream::parse(&format!("http://{}{prefix}", self.addr)).unwrap()
    }

    pub(crate) fn requests(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }
}

/// A proxy for `upstream`, serving in the background, and its events.
pub(crate) async fn proxy(upstream: Upstream) -> (SocketAddr, UnboundedReceiver<Event>) {
    let (events, received) = tokio::sync::mpsc::unbounded_channel();
    let proxy = Proxy::bind(upstream, events).await.unwrap();
    let addr = proxy.local_addr().unwrap();
    tokio::spawn(proxy.serve());
    (addr, received)
}

/// The events the proxy reported so far.
pub(crate) fn drain(events: &mut UnboundedReceiver<Event>) -> Vec<Event> {
    std::iter::from_fn(|| events.try_recv().ok()).collect()
}

/// What the agent receives for `request` sent to the proxy at `addr`.
pub(crate) async fn send(
    addr: SocketAddr,
    request: hyper::http::request::Builder,
    body: &str,
) -> (StatusCode, HeaderMap, String) {
    let client: Client<HttpConnector, Full<Bytes>> =
        Client::builder(TokioExecutor::new()).build_http();
    let mut request = request
        .body(Full::new(Bytes::from(body.to_owned())))
        .unwrap();
    let uri = format!("http://{addr}{}", request.uri());
    *request.uri_mut() = uri.parse().unwrap();
    let response = client.request(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let body = body.collect().await.unwrap().to_bytes();
    (
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&body).into_owned(),
    )
}

pub(crate) fn reply(status: u16, body: &str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body.to_owned())));
    *response.status_mut() = StatusCode::from_u16(status).unwrap();
    response
}

#[tokio::test]
async fn requests_reach_the_upstream_with_their_own_credentials() {
    let upstream = FakeUpstream::start(|_| {
        let mut response = reply(201, "created");
        response
            .headers_mut()
            .insert("x-upstream", "yes".parse().unwrap());
        response
    })
    .await;
    let (addr, mut events) = proxy(upstream.url("/prefix")).await;
    let (status, headers, body) = send(
        addr,
        Request::post("/v1/messages?beta=true")
            .header("x-api-key", "secret")
            .header("connection", "keep-alive"),
        r#"{"model":"m"}"#,
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::CREATED, "created"));
    assert_eq!(headers["x-upstream"], "yes");
    let requests = upstream.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/prefix/v1/messages?beta=true");
    assert_eq!(requests[0].headers["x-api-key"], "secret");
    assert_eq!(requests[0].headers["host"], upstream.addr.to_string());
    assert_eq!(requests[0].body, r#"{"model":"m"}"#);
    assert!(drain(&mut events).is_empty());
}

#[tokio::test(start_paused = true)]
async fn transient_failures_are_retried_until_the_upstream_recovers() {
    let upstream = FakeUpstream::start(|received| match received.len() {
        1 => reply(529, "overloaded"),
        2 => {
            let mut response = reply(429, "slow down");
            response
                .headers_mut()
                .insert("retry-after", "7".parse().unwrap());
            response
        }
        _ => reply(200, "done"),
    })
    .await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (status, _, body) = send(addr, Request::post("/v1/messages"), "{}").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "done"));
    assert_eq!(upstream.requests().len(), 3);
    assert_eq!(
        drain(&mut events),
        [
            Event::Retrying {
                failure: Failure::Overloaded,
                attempt: 1,
                wait: Duration::from_secs(1),
            },
            Event::Retrying {
                failure: Failure::RateLimited,
                attempt: 2,
                wait: Duration::from_secs(7),
            },
            Event::Recovered,
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn an_outage_past_its_budget_reaches_the_agent_at_once() {
    let upstream = FakeUpstream::start(|_| reply(503, "busy")).await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (status, _, body) = send(addr, Request::post("/v1/messages"), "{}").await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "busy")
    );
    let events = drain(&mut events);
    assert_eq!(
        events.last(),
        Some(&Event::Failed {
            failure: Failure::Overloaded,
            in_stream: false
        })
    );
    let waited: Duration = events
        .iter()
        .filter_map(|event| match event {
            Event::Retrying { wait, .. } => Some(*wait),
            _ => None,
        })
        .sum();
    assert!(waited <= retry::OUTAGE_BUDGET, "{waited:?}");
    // The outage outlasted its budget, so the next request fails at once.
    let attempts = upstream.requests().len();
    send(addr, Request::post("/v1/messages"), "{}").await;
    assert_eq!(upstream.requests().len(), attempts + 1);
}

#[tokio::test]
async fn failures_a_retry_cannot_fix_reach_the_agent_at_once() {
    let upstream = FakeUpstream::start(|_| reply(401, "bad key")).await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (status, _, body) = send(addr, Request::post("/v1/messages"), "{}").await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::UNAUTHORIZED, "bad key")
    );
    assert_eq!(upstream.requests().len(), 1);
    assert!(drain(&mut events).is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_unreachable_upstream_is_retried_then_answers_bad_gateway() {
    // A port nothing listens on any more.
    let closed = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let (addr, mut events) = proxy(Upstream::parse(&url).unwrap()).await;
    let (status, _, body) = send(addr, Request::post("/v1/messages"), "{}").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("reach the upstream"), "{body}");
    assert_eq!(
        drain(&mut events).last(),
        Some(&Event::Failed {
            failure: Failure::Unreachable,
            in_stream: false
        })
    );
}

/// A successful stream of `events`.
fn sse(events: &str) -> Response<Full<Bytes>> {
    let mut response = reply(200, events);
    response
        .headers_mut()
        .insert("content-type", "text/event-stream".parse().unwrap());
    response
}

const MESSAGE_START: &str = "event: message_start\ndata: {\"type\":\"message_start\"}\n\n\
    event: ping\ndata: {\"type\":\"ping\"}\n\n";
const ANTHROPIC_OVERLOADED: &str =
    "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}\n\n";
const ANTHROPIC_CONTENT: &str = "event: content_block_start\ndata: {\"type\":\"content_block_start\"}\n\n\
    event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
const RESPONSE_CREATED: &str = "event: response.created\ndata: {\"type\":\"response.created\"}\n\n";
const RESPONSE_OVERLOADED: &str = "event: response.failed\ndata: \
    {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_is_overloaded\"}}}\n\n";
const RESPONSE_CONTENT: &str = "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\"}\n\n\
    event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n";

#[tokio::test(start_paused = true)]
async fn an_overload_before_any_content_is_retried_unseen() {
    let opening = format!("{MESSAGE_START}{ANTHROPIC_OVERLOADED}");
    let recovered = format!("{MESSAGE_START}{ANTHROPIC_CONTENT}");
    let reply_with = recovered.clone();
    let upstream = FakeUpstream::start(move |received| match received.len() {
        1 => sse(&opening),
        _ => sse(&reply_with),
    })
    .await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (status, headers, body) =
        send(addr, Request::post("/v1/messages"), r#"{"stream":true}"#).await;
    assert_eq!((status, body), (StatusCode::OK, recovered));
    assert_eq!(headers["content-type"], "text/event-stream");
    let requests = upstream.requests();
    assert_eq!(requests.len(), 2);
    // The proxy reads the stream, so it asks for it uncompressed.
    assert_eq!(requests[0].headers["accept-encoding"], "identity");
    assert_eq!(
        drain(&mut events),
        [
            Event::Retrying {
                failure: Failure::Overloaded,
                attempt: 1,
                wait: Duration::from_secs(1),
            },
            Event::Recovered,
        ]
    );

    let upstream = FakeUpstream::start(|received| match received.len() {
        1 => sse(&format!("{RESPONSE_CREATED}{RESPONSE_OVERLOADED}")),
        _ => sse(&format!("{RESPONSE_CREATED}{RESPONSE_CONTENT}")),
    })
    .await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (status, _, body) = send(addr, Request::post("/responses"), r#"{"stream":true}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, format!("{RESPONSE_CREATED}{RESPONSE_CONTENT}"));
    assert_eq!(drain(&mut events).last(), Some(&Event::Recovered));
}

#[tokio::test]
async fn a_failure_after_content_reaches_the_agent_as_it_came() {
    let stream = format!("{MESSAGE_START}{ANTHROPIC_CONTENT}{ANTHROPIC_OVERLOADED}");
    let reply_with = stream.clone();
    let upstream = FakeUpstream::start(move |_| sse(&reply_with)).await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (_, _, body) = send(addr, Request::post("/v1/messages"), r#"{"stream":true}"#).await;
    assert_eq!(body, stream);
    assert_eq!(upstream.requests().len(), 1);
    assert!(drain(&mut events).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_streaming_codex_request_ends_its_turn_once_the_proxy_gives_up() {
    // An HTTP failure, which Codex would retry itself, arrives in the stream.
    let upstream = FakeUpstream::start(|_| reply(503, "busy")).await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (status, headers, body) =
        send(addr, Request::post("/responses"), r#"{"stream":true}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/event-stream");
    assert!(body.starts_with("event: response.failed\n"), "{body}");
    assert!(body.contains(r#""code":"server_is_overloaded""#), "{body}");
    assert_eq!(
        drain(&mut events).last(),
        Some(&Event::Failed {
            failure: Failure::Overloaded,
            in_stream: true
        })
    );

    // A failure that already arrived in the stream reaches it as it came.
    let opening = format!("{RESPONSE_CREATED}{RESPONSE_OVERLOADED}");
    let reply_with = opening.clone();
    let upstream = FakeUpstream::start(move |_| sse(&reply_with)).await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (_, _, body) = send(addr, Request::post("/responses"), r#"{"stream":true}"#).await;
    assert_eq!(body, opening);
    assert_eq!(
        drain(&mut events).last(),
        Some(&Event::Failed {
            failure: Failure::Overloaded,
            in_stream: true
        })
    );

    // Claude Code handles an HTTP failure itself, so it keeps it.
    let upstream = FakeUpstream::start(|_| reply(529, "overloaded")).await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (status, _, _) = send(addr, Request::post("/v1/messages"), r#"{"stream":true}"#).await;
    assert_eq!(status.as_u16(), 529);
    assert_eq!(
        drain(&mut events).last(),
        Some(&Event::Failed {
            failure: Failure::Overloaded,
            in_stream: false
        })
    );
}
