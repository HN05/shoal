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
