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
use crate::clock;

/// A request as the upstream received it.
#[derive(Debug, Clone)]
pub(crate) struct Received {
    pub path: String,
    pub headers: HeaderMap,
    pub body: String,
}

/// What the fake upstream does with a request.
pub(crate) enum Reply {
    Answer(Response<Full<Bytes>>),
    /// Never answer.
    Stall,
    /// Answer with this status and a body that never ends.
    StallBody(u16),
}

impl From<Response<Full<Bytes>>> for Reply {
    fn from(response: Response<Full<Bytes>>) -> Self {
        Self::Answer(response)
    }
}

/// An upstream that answers each request with what `reply` returns for the
/// requests it has received so far, the current one last.
pub(crate) struct FakeUpstream {
    pub addr: SocketAddr,
    pub received: Arc<Mutex<Vec<Received>>>,
    arrived: Arc<tokio::sync::Notify>,
}

impl FakeUpstream {
    pub(crate) async fn start<R: Into<Reply>>(
        reply: impl Fn(&[Received]) -> R + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let reply = Arc::new(reply);
        let log = received.clone();
        let arrived = Arc::new(tokio::sync::Notify::new());
        let arriving = arrived.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (reply, log, arriving) = (reply.clone(), log.clone(), arriving.clone());
                let service = service_fn(move |request: Request<Incoming>| {
                    let (reply, log, arriving) = (reply.clone(), log.clone(), arriving.clone());
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = body.collect().await.unwrap().to_bytes();
                        let answer = {
                            let mut log = log.lock().unwrap();
                            log.push(Received {
                                path: parts.uri.to_string(),
                                headers: parts.headers,
                                body: String::from_utf8_lossy(&body).into_owned(),
                            });
                            reply(&log).into()
                        };
                        arriving.notify_waiters();
                        match answer {
                            Reply::Answer(response) => {
                                Ok::<_, Infallible>(response.map(|body| {
                                    body.map_err(|never| match never {}).boxed_unsync()
                                }))
                            }
                            Reply::Stall => std::future::pending().await,
                            Reply::StallBody(status) => {
                                let never = futures_util::stream::pending::<
                                    Result<hyper::body::Frame<Bytes>, Infallible>,
                                >();
                                let mut response = Response::new(
                                    http_body_util::StreamBody::new(never).boxed_unsync(),
                                );
                                *response.status_mut() = StatusCode::from_u16(status).unwrap();
                                Ok(response)
                            }
                        }
                    }
                });
                tokio::spawn(http1::Builder::new().serve_connection(TokioIo::new(stream), service));
            }
        });
        Self {
            addr,
            received,
            arrived,
        }
    }

    /// Wait until the upstream has received `count` requests.
    pub(crate) async fn received(&self, count: usize) {
        loop {
            let arrived = self.arrived.notified();
            if self.requests().len() >= count {
                return;
            }
            arrived.await;
        }
    }

    pub(crate) fn url(&self, prefix: &str) -> Upstream {
        Upstream::parse(&format!("http://{}{prefix}", self.addr)).unwrap()
    }

    pub(crate) fn requests(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }
}

/// Waits short enough for a test, with a budget no test reaches.
const RETRYING: retry::Policy = retry::Policy {
    first_wait: Duration::from_millis(10),
    max_wait: Duration::from_millis(40),
    budget: Duration::from_secs(60),
};

/// A proxy for `upstream`, serving in the background, and its events.
pub(crate) async fn proxy(upstream: Upstream) -> (SocketAddr, UnboundedReceiver<Event>) {
    proxy_with(upstream, RETRYING, Arc::new(clock::Tokio::new())).await
}

async fn proxy_with(
    upstream: Upstream,
    policy: retry::Policy,
    clock: Arc<dyn clock::Clock>,
) -> (SocketAddr, UnboundedReceiver<Event>) {
    let (events, received) = tokio::sync::mpsc::unbounded_channel();
    let proxy = Proxy::bind_with(upstream, events, policy, clock)
        .await
        .unwrap();
    let addr = proxy.local_addr().unwrap();
    tokio::spawn(proxy.serve());
    (addr, received)
}

/// A proxy for `upstream` whose waits and budget pass only as the returned
/// clock is advanced, for tests in which the proxy gives up.
async fn proxy_on(
    upstream: Upstream,
) -> (SocketAddr, UnboundedReceiver<Event>, Arc<clock::Manual>) {
    let clock = Arc::new(clock::Manual::default());
    let (addr, events) = proxy_with(upstream, retry::POLICY, clock.clone()).await;
    (addr, events, clock)
}

/// Advance `clock` whenever the proxy waits on it until `request` is
/// answered.
async fn drive<T>(clock: &clock::Manual, request: impl std::future::Future<Output = T>) -> T {
    tokio::pin!(request);
    loop {
        tokio::select! {
            answer = &mut request => return answer,
            () = clock.advance() => {}
        }
    }
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

#[tokio::test]
async fn transient_failures_are_retried_until_the_upstream_recovers() {
    let upstream = FakeUpstream::start(|received| match received.len() {
        1 => reply(529, "overloaded"),
        2 => reply(429, "slow down"),
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
                wait: RETRYING.first_wait,
            },
            Event::Retrying {
                failure: Failure::RateLimited,
                attempt: 2,
                wait: RETRYING.first_wait * 2,
            },
            Event::Recovered,
        ]
    );
}

#[tokio::test]
async fn an_outage_past_its_budget_reaches_the_agent_at_once() {
    let upstream = FakeUpstream::start(|_| reply(503, "busy")).await;
    let (addr, mut events, clock) = proxy_on(upstream.url("")).await;
    let (status, _, body) = drive(&clock, send(addr, Request::post("/v1/messages"), "{}")).await;
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
    assert!(waited <= retry::POLICY.budget, "{waited:?}");
    // The outage outlasted its budget, so the next request fails at once.
    let attempts = upstream.requests().len();
    drive(&clock, send(addr, Request::post("/v1/messages"), "{}")).await;
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

#[tokio::test]
async fn an_unreachable_upstream_is_retried_then_answers_bad_gateway() {
    // A port nothing listens on any more.
    let closed = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let (addr, mut events, clock) = proxy_on(Upstream::parse(&url).unwrap()).await;
    let (status, _, body) = drive(&clock, send(addr, Request::post("/v1/messages"), "{}")).await;
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

#[tokio::test]
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
                wait: RETRYING.first_wait,
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

#[tokio::test]
async fn a_streaming_codex_request_ends_its_turn_once_the_proxy_gives_up() {
    // An HTTP failure, which Codex would retry itself, arrives in the stream.
    let upstream = FakeUpstream::start(|_| reply(503, "busy")).await;
    let (addr, mut events, clock) = proxy_on(upstream.url("")).await;
    let (status, headers, body) = drive(
        &clock,
        send(addr, Request::post("/responses"), r#"{"stream":true}"#),
    )
    .await;
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
    let (addr, mut events, clock) = proxy_on(upstream.url("")).await;
    let (_, _, body) = drive(
        &clock,
        send(addr, Request::post("/responses"), r#"{"stream":true}"#),
    )
    .await;
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
    let (addr, mut events, clock) = proxy_on(upstream.url("")).await;
    let (status, _, _) = drive(
        &clock,
        send(addr, Request::post("/v1/messages"), r#"{"stream":true}"#),
    )
    .await;
    assert_eq!(status.as_u16(), 529);
    assert_eq!(
        drain(&mut events).last(),
        Some(&Event::Failed {
            failure: Failure::Overloaded,
            in_stream: false
        })
    );
}

#[tokio::test]
async fn a_retry_the_upstream_never_answers_gives_up_with_the_outage() {
    let upstream = FakeUpstream::start(|received| match received.len() {
        1 => Reply::from(reply(503, "busy")),
        _ => Reply::Stall,
    })
    .await;
    let (addr, mut events, clock) = proxy_on(upstream.url("")).await;
    let request = tokio::spawn(send(addr, Request::post("/v1/messages"), "{}"));
    // The first attempt fails, and the retry after its wait never answers.
    drive(&clock, upstream.received(2)).await;
    clock.expire().await;
    let (status, _, body) = request.await.unwrap();
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("did not answer"), "{body}");
    assert_eq!(
        drain(&mut events).last(),
        Some(&Event::Failed {
            failure: Failure::Unreachable,
            in_stream: false
        })
    );
}

#[tokio::test]
async fn a_stream_that_ends_before_content_is_retried() {
    let complete = format!("{MESSAGE_START}{ANTHROPIC_CONTENT}");
    let reply_with = complete.clone();
    let upstream = FakeUpstream::start(move |received| match received.len() {
        1 => sse(""),
        2 => sse(MESSAGE_START),
        _ => sse(&reply_with),
    })
    .await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (_, _, body) = send(addr, Request::post("/v1/messages"), r#"{"stream":true}"#).await;
    assert_eq!(body, complete);
    let events = drain(&mut events);
    assert_eq!(events.len(), 3, "{events:?}");
    assert!(matches!(
        events[0],
        Event::Retrying {
            failure: Failure::ServerError,
            ..
        }
    ));
}

#[tokio::test]
async fn a_comment_that_keeps_a_stream_alive_is_no_content() {
    let opening = format!(": keep-alive\n\n{MESSAGE_START}{ANTHROPIC_OVERLOADED}");
    let complete = format!("{MESSAGE_START}{ANTHROPIC_CONTENT}");
    let reply_with = complete.clone();
    let upstream = FakeUpstream::start(move |received| match received.len() {
        1 => sse(&opening),
        _ => sse(&reply_with),
    })
    .await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (_, _, body) = send(addr, Request::post("/v1/messages"), r#"{"stream":true}"#).await;
    assert_eq!(body, complete);
    assert_eq!(drain(&mut events).last(), Some(&Event::Recovered));
}

#[tokio::test]
async fn a_stream_that_keeps_ending_early_reaches_the_agent_as_a_failure() {
    let upstream = FakeUpstream::start(|_| sse(MESSAGE_START)).await;
    let (addr, mut events, clock) = proxy_on(upstream.url("")).await;
    // Claude Code gets an error, not an incomplete answer.
    let request = send(addr, Request::post("/v1/messages"), r#"{"stream":true}"#);
    let (status, _, body) = drive(&clock, request).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("ended before any content"), "{body}");
    assert_eq!(
        drain(&mut events).last(),
        Some(&Event::Failed {
            failure: Failure::ServerError,
            in_stream: false
        })
    );

    // A streaming Codex request ends its turn.
    let upstream = FakeUpstream::start(|_| sse(RESPONSE_CREATED)).await;
    let (addr, mut events, clock) = proxy_on(upstream.url("")).await;
    let request = send(addr, Request::post("/responses"), r#"{"stream":true}"#);
    let (status, _, body) = drive(&clock, request).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#""code":"server_error""#), "{body}");
    assert_eq!(
        drain(&mut events).last(),
        Some(&Event::Failed {
            failure: Failure::ServerError,
            in_stream: true
        })
    );
}

#[tokio::test]
async fn a_usage_limit_ends_the_turn_at_once_with_the_upstreams_own_error() {
    // The limit resets in two hours, past the proxy's budget.
    let upstream = FakeUpstream::start(|_| {
        reply(
            429,
            r#"{"error":{"type":"usage_limit_reached","message":"You've hit your usage limit.","resets_in_seconds":7200}}"#,
        )
    })
    .await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (status, _, body) = send(addr, Request::post("/responses"), r#"{"stream":true}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#""code":"usage_limit_reached""#), "{body}");
    assert!(body.contains("You've hit your usage limit."), "{body}");
    assert_eq!(upstream.requests().len(), 1);
    assert_eq!(
        drain(&mut events),
        [Event::Failed {
            failure: Failure::RateLimited,
            in_stream: true
        }]
    );

    // Claude Code gets the 429 as it came, at once.
    let upstream = FakeUpstream::start(|_| {
        let mut response = reply(
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error"}}"#,
        );
        response
            .headers_mut()
            .insert("retry-after", "7200".parse().unwrap());
        response
    })
    .await;
    let (addr, _) = proxy(upstream.url("")).await;
    let (status, _, body) = send(addr, Request::post("/v1/messages"), r#"{"stream":true}"#).await;
    assert_eq!(status.as_u16(), 429);
    assert!(body.contains("rate_limit_error"), "{body}");
    assert_eq!(upstream.requests().len(), 1);
}

#[tokio::test]
async fn a_usage_limit_inside_the_stream_ends_the_turn_at_once() {
    let opening = format!(
        "{RESPONSE_CREATED}event: response.failed\ndata: {}\n\n",
        r#"{"type":"response.failed","response":{"error":{"code":"usage_limit_reached","message":"You've hit your usage limit.","resets_in_seconds":7200}}}"#
    );
    let reply_with = opening.clone();
    let upstream = FakeUpstream::start(move |_| sse(&reply_with)).await;
    let (addr, mut events) = proxy(upstream.url("")).await;
    let (_, _, body) = send(addr, Request::post("/responses"), r#"{"stream":true}"#).await;
    assert_eq!(body, opening);
    // The reset is past the budget, so the proxy does not retry it.
    assert_eq!(upstream.requests().len(), 1);
    assert_eq!(
        drain(&mut events),
        [Event::Failed {
            failure: Failure::RateLimited,
            in_stream: true
        }]
    );
}

#[tokio::test]
async fn a_failure_whose_body_never_ends_is_retried_by_its_status() {
    let upstream = FakeUpstream::start(|received| match received.len() {
        1 => Reply::StallBody(503),
        _ => Reply::from(reply(200, "done")),
    })
    .await;
    let (addr, _events, clock) = proxy_on(upstream.url("")).await;
    let request = tokio::spawn(send(addr, Request::post("/v1/messages"), "{}"));
    upstream.received(1).await;
    // The proxy stops waiting for the body, then retries the 503.
    clock.expire().await;
    let (status, _, body) = drive(&clock, async { request.await.unwrap() }).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "done"));
}
