use std::{sync::Arc, time::Duration};

use hyper::{Request, StatusCode};

use super::*;
use crate::{
    clock, retry,
    tests::{FakeUpstream, Reply, reply, send},
};

/// A forwarder serving on a loopback port, and its address.
async fn serve(forwarder: Arc<Forwarder>) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(forwarder.clone().serve_connection(stream));
        }
    });
    addr
}

#[tokio::test]
async fn a_retry_gives_up_on_an_outage_another_request_gave_up_on() {
    // The retry would stall if the proxy sent it.
    let upstream = FakeUpstream::start(|received| match received.len() {
        1 => Reply::from(reply(503, "busy")),
        _ => Reply::Stall,
    })
    .await;
    let clock = Arc::new(clock::Manual::default());
    let (events, _received) = tokio::sync::mpsc::unbounded_channel();
    let forwarder = Arc::new(Forwarder::new(
        upstream.url(""),
        None,
        events,
        retry::POLICY,
        clock.clone(),
    ));
    let addr = serve(forwarder.clone()).await;
    let request = tokio::spawn(send(addr, Request::post("/v1/messages"), "{}"));
    upstream.received(1).await;
    clock.waiting().await;
    // Another request gives up on the outage while this one waits to retry.
    let asked = Some(retry::POLICY.budget * 2);
    assert_eq!(forwarder.outage.next_wait(1, asked), None);
    clock.advance().await;
    // A generous guard: the request gives up as soon as its wait ends.
    let answer = tokio::time::timeout(Duration::from_secs(60), request);
    let (status, _, body) = answer.await.unwrap().unwrap();
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "busy")
    );
    assert_eq!(upstream.requests().len(), 1);
}

#[tokio::test]
async fn a_retry_resends_once_another_request_found_the_upstream_back() {
    let upstream = FakeUpstream::start(|received| match received.len() {
        1 => reply(503, "busy"),
        _ => reply(200, "done"),
    })
    .await;
    let clock = Arc::new(clock::Manual::default());
    let (events, _received) = tokio::sync::mpsc::unbounded_channel();
    let forwarder = Arc::new(Forwarder::new(
        upstream.url(""),
        None,
        events,
        retry::POLICY,
        clock.clone(),
    ));
    let addr = serve(forwarder.clone()).await;
    let request = tokio::spawn(send(addr, Request::post("/v1/messages"), "{}"));
    upstream.received(1).await;
    clock.waiting().await;
    // Another request succeeds while this one waits to retry.
    forwarder.outage.ended();
    clock.advance().await;
    // A generous guard: the retry goes out as soon as its wait ends.
    let answer = tokio::time::timeout(Duration::from_secs(60), request);
    let (status, _, body) = answer.await.unwrap().unwrap();
    assert_eq!((status, body.as_str()), (StatusCode::OK, "done"));
    assert_eq!(upstream.requests().len(), 2);
}
