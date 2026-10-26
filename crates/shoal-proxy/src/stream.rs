//! Failures inside a response stream. Anthropic Messages and OpenAI
//! Responses streams may open with a few events, then report an overload
//! as an event instead of an HTTP status; until content arrives the proxy
//! can still retry the request without the agent seeing it.
use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use serde_json::{Value, json};

use crate::Failure;

/// The API a request speaks, which says what its stream events mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Protocol {
    AnthropicMessages,
    OpenAiResponses,
}

impl Protocol {
    /// The protocol of a request to `path`; `None` for one whose stream the
    /// proxy does not read.
    pub(crate) fn of(path: &str) -> Option<Self> {
        if path.ends_with("/messages") {
            Some(Self::AnthropicMessages)
        } else if path.ends_with("/responses") {
            Some(Self::OpenAiResponses)
        } else {
            None
        }
    }

    /// What `event` says, from its name or else its data's `type`. A record
    /// with neither, such as a comment that keeps the stream alive, opens
    /// nothing.
    fn classify(self, name: &str, data: &Value) -> Kind {
        if name.is_empty() && data.is_null() {
            return Kind::Preamble;
        }
        let name = if name.is_empty() {
            data["type"].as_str().unwrap_or_default()
        } else {
            name
        };
        match (self, name) {
            (Self::AnthropicMessages, "message_start" | "ping") => Kind::Preamble,
            (Self::AnthropicMessages, "error") => {
                Kind::Failure(anthropic_failure(data["error"]["type"].as_str()))
            }
            (
                Self::OpenAiResponses,
                "response.created" | "response.in_progress" | "response.queued",
            ) => Kind::Preamble,
            (Self::OpenAiResponses, "response.failed") => {
                Kind::Failure(openai_failure(data["response"]["error"]["code"].as_str()))
            }
            (Self::OpenAiResponses, "error") => {
                let code = data["code"].as_str().or(data["error"]["code"].as_str());
                Kind::Failure(openai_failure(code))
            }
            _ => Kind::Content,
        }
    }

    /// A stream that reports `failure`, which ends the agent's turn at once
    /// where an HTTP failure would start the agent's own retries; `None`
    /// where the agent handles the HTTP failure itself.
    pub(crate) fn failure_stream(self, failure: Failure) -> Option<Bytes> {
        let code = match failure {
            Failure::RateLimited => "rate_limit_exceeded",
            Failure::Overloaded | Failure::Unreachable => "server_is_overloaded",
            Failure::ServerError => "server_error",
            Failure::Other => return None,
        };
        match self {
            Self::AnthropicMessages => None,
            Self::OpenAiResponses => {
                let data = json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_shoal_proxy",
                        "object": "response",
                        "status": "failed",
                        "error": {
                            "code": code,
                            "message": format!("shoal-proxy gave up retrying: {code}"),
                        },
                    },
                });
                Some(Bytes::from(format!(
                    "event: response.failed\ndata: {data}\n\n"
                )))
            }
        }
    }
}

/// What one stream event says about the request.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    /// It opens the stream before any content.
    Preamble,
    /// It carries content, after which the stream goes to the agent as is.
    Content,
    Failure(Failure),
}

fn anthropic_failure(error: Option<&str>) -> Failure {
    match error {
        Some("overloaded_error") => Failure::Overloaded,
        Some("rate_limit_error") => Failure::RateLimited,
        Some("api_error") => Failure::ServerError,
        _ => Failure::Other,
    }
}

fn openai_failure(code: Option<&str>) -> Failure {
    match code {
        Some("server_is_overloaded" | "slow_down") => Failure::Overloaded,
        Some("rate_limit_exceeded") => Failure::RateLimited,
        Some("server_error" | "internal_error") => Failure::ServerError,
        _ => Failure::Other,
    }
}

/// How a stream opened, with every byte read from it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Opening {
    /// Content arrived; the rest of the stream follows.
    Content(Bytes),
    /// A failure arrived before any content.
    Failure(Failure, Bytes),
    /// The stream ended without either, so it is incomplete.
    Ended(Bytes),
}

/// Read `body`'s opening events until content, a failure or its end.
pub(crate) async fn open(protocol: Protocol, body: &mut Incoming) -> Result<Opening> {
    let mut read = BytesMut::new();
    let mut parsed = 0;
    loop {
        while let Some((end, name, data)) = next_event(&read[parsed..]) {
            parsed += end;
            match protocol.classify(&name, &data) {
                Kind::Preamble => {}
                Kind::Content => return Ok(Opening::Content(read.freeze())),
                Kind::Failure(failure) => return Ok(Opening::Failure(failure, read.freeze())),
            }
        }
        let Some(frame) = body.frame().await else {
            return Ok(Opening::Ended(read.freeze()));
        };
        let frame = frame.context("read the upstream's stream")?;
        if let Ok(data) = frame.into_data() {
            read.extend_from_slice(&data);
        }
    }
}

/// The first complete event in `buffer`: where it ends, its name and its
/// data; `None` until a blank line ends one.
fn next_event(buffer: &[u8]) -> Option<(usize, String, Value)> {
    let text = std::str::from_utf8(buffer).ok().or_else(|| {
        // A multibyte character may be split across frames.
        let valid = std::str::from_utf8(buffer).err()?.valid_up_to();
        std::str::from_utf8(&buffer[..valid]).ok()
    })?;
    let (event, end) = [("\r\n\r\n", 4), ("\n\n", 2)]
        .into_iter()
        .filter_map(|(separator, len)| text.find(separator).map(|at| (at, at + len)))
        .min()?;
    let (mut name, mut data) = (String::new(), String::new());
    for line in text[..event].lines() {
        if let Some(value) = line.strip_prefix("event:") {
            name = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.trim_start());
        }
    }
    Some((
        end,
        name,
        serde_json::from_str(&data).unwrap_or(Value::Null),
    ))
}

#[cfg(test)]
mod tests;
