//! What the agent receives: the upstream's answer, or one the proxy makes.
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use http_body_util::{BodyExt, BodyStream, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    HeaderMap, Response, StatusCode,
    body::{Frame, Incoming},
    header::{self, HeaderValue},
};

use crate::request::HOP_BY_HOP;

pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;
/// What the proxy answers the agent with.
pub(crate) type Body = UnsyncBoxBody<Bytes, BoxError>;

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

pub(crate) fn full(bytes: Bytes) -> Body {
    Full::new(bytes).map_err(BoxError::from).boxed_unsync()
}

/// The bytes already read from a stream, then the rest of it as it comes.
pub(crate) fn prefixed(read: Bytes, rest: Incoming) -> Body {
    let read = futures_util::stream::once(std::future::ready(Ok(Frame::data(read))));
    let rest = TryStreamExt::map_err(BodyStream::new(rest), BoxError::from);
    StreamBody::new(read.chain(rest)).boxed_unsync()
}

/// A successful response carrying `stream`'s events.
pub(crate) fn event_stream(stream: Bytes) -> Response<Body> {
    let mut response = Response::new(full(stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

/// The answer to a client that does not name the proxy's key.
pub(crate) fn forbidden() -> Response<Body> {
    let mut response = Response::new(full(Bytes::from_static(
        b"shoal-proxy: the request does not name this proxy's key\n",
    )));
    *response.status_mut() = StatusCode::FORBIDDEN;
    response
}

/// The answer when the proxy could not reach the upstream at all.
pub(crate) fn bad_gateway(error: &anyhow::Error) -> Response<Body> {
    let mut response = Response::new(full(Bytes::from(format!("shoal-proxy: {error:#}\n"))));
    *response.status_mut() = StatusCode::BAD_GATEWAY;
    response
}

pub(crate) fn is_event_stream(response: &Response<Incoming>) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"))
}
