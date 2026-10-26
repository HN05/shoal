//! Forwarding one agent request to the upstream and its response back.
use std::{convert::Infallible, sync::Arc};

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
use tokio::net::TcpStream;

use crate::Upstream;

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
}

impl Forwarder {
    pub(crate) fn new(upstream: Upstream) -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Self {
            upstream,
            client: Client::builder(TokioExecutor::new()).build(connector),
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
        match self.forward(request).await {
            Ok(response) => response,
            Err(error) => bad_gateway(&error),
        }
    }

    async fn forward(&self, request: Request<Incoming>) -> Result<Response<Body>> {
        let request = Buffered::read(request).await?;
        let response = self
            .client
            .request(request.to_upstream(&self.upstream)?)
            .await
            .context("reach the upstream")?;
        Ok(passthrough(response))
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
