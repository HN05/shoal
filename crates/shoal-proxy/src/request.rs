//! The agent's request, read whole so the proxy can send it again, and as
//! the upstream receives it.
use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Method, Request,
    body::Incoming,
    header::{self, HeaderName, HeaderValue},
};

use crate::{Upstream, retry::Failure, stream::Protocol};

/// Headers that describe one connection, not the request, so they are
/// never forwarded.
pub(crate) const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
];

/// Headers that carry the agent's credentials, which an upstream key
/// replaces.
const CREDENTIALS: [HeaderName; 2] = [header::AUTHORIZATION, HeaderName::from_static("x-api-key")];

/// An agent request read whole, so the proxy can send it again.
pub(crate) struct Buffered {
    method: Method,
    path_and_query: String,
    headers: HeaderMap,
    body: Bytes,
    /// The API it speaks, when the proxy reads its response stream.
    pub(crate) protocol: Option<Protocol>,
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
            protocol: Protocol::of(parts.uri.path()),
            path_and_query: parts
                .uri
                .path_and_query()
                .map_or("/", |path| path.as_str())
                .to_owned(),
            headers: parts.headers,
            body,
        })
    }

    /// The stream that reports `failure` to an agent that streams this
    /// request, as its protocol does.
    pub(crate) fn failure_stream(
        &self,
        failure: Failure,
        error: Option<&serde_json::Value>,
    ) -> Option<Bytes> {
        let request: serde_json::Value = serde_json::from_slice(&self.body).ok()?;
        if request["stream"] != true {
            return None;
        }
        self.protocol?.failure_stream(failure, error)
    }

    /// This request addressed to `upstream`, with its own headers but the
    /// upstream's `Host`, and the upstream's key when it has one.
    pub(crate) fn to_upstream(&self, upstream: &Upstream) -> Result<Request<Full<Bytes>>> {
        let mut request = Request::builder()
            .method(self.method.clone())
            .uri(upstream.join(&self.path_and_query)?)
            .body(Full::new(self.body.clone()))?;
        let headers = request.headers_mut();
        let key = upstream.key();
        for (name, value) in &self.headers {
            let replaced = key.is_some() && CREDENTIALS.contains(name);
            if forwarded(name)
                && !replaced
                && name != header::HOST
                && name != header::CONTENT_LENGTH
            {
                headers.append(name, value.clone());
            }
        }
        headers.insert(header::HOST, HeaderValue::from_str(upstream.authority())?);
        if let Some(key) = key {
            headers.insert(header::AUTHORIZATION, key.clone());
        }
        // The proxy reads the stream, so it must arrive uncompressed.
        if self.protocol.is_some() {
            headers.insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_static("identity"),
            );
        }
        Ok(request)
    }
}

fn forwarded(name: &HeaderName) -> bool {
    !HOP_BY_HOP.contains(&name.as_str())
}
