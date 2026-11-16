//! The `webhook` source: HTTP requests to the daemon's webhook listener at
//! the trigger's route, signed with a secret where the trigger names one.
//! A request's input document holds what it sent, which a task marks as
//! data, and a delivery ID header is the key of a trigger that sets none,
//! so a redelivery is a duplicate.
use anyhow::{Result, bail, ensure};
use jiff::{Timestamp, tz::TimeZone};
use serde_json::{Map, Value, json};
use shoal_protocol::triggers::WebhookRequest;
use shoal_states::states;

use super::input::{Input, Request};
use crate::time::UnixTime;


/// The input document's keys that hold what a request sent.
const REQUEST_ROOTS: &[&str] = &["payload", "body", "headers", "query"];
/// Its tables of names, which a template listing whole redacts.
const LISTINGS: &[&str] = &["headers", "query"];
/// Headers that name a delivery, which a redelivery repeats.
const DELIVERY_HEADERS: &[&str] = &[
    "x-forgejo-delivery",
    "x-gitea-delivery",
    "x-github-delivery",
];

/// What a webhook trigger accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Webhook {
    /// How a request proves it knows the secret; none accepts any request
    /// that reaches the listener.
    pub signed: Option<Signed>,
}

/// A webhook's secret and where requests carry their signature with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    /// The `[secrets]` entry whose value signs requests.
    pub secret: String,
    pub signature: Signature,
}

/// Where a request carries the HMAC-SHA256 of its body, in hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signature {
    /// `X-Forgejo-Signature`.
    Forgejo,
    /// `X-Hub-Signature-256`, after `sha256=`.
    Github,
    /// The header named, lowercased, after `sha256=` or not.
    Header(String),
}

states!(
    /// `signature` as config writes it.
    Scheme {
        Forgejo => "forgejo",
        Github => "github",
        HmacSha256 => "hmac-sha256",
    }
);

impl Webhook {
    /// A webhook whose requests sign with the `[secrets]` entry `secret`,
    /// carrying the signature as `signature` says, in `signature_header`
    /// for `hmac-sha256`; without a secret, it takes neither.
    pub fn new(
        secret: Option<String>,
        signature: Option<Scheme>,
        signature_header: Option<String>,
    ) -> Result<Self> {
        let Some(secret) = secret else {
            ensure!(
                signature.is_none() && signature_header.is_none(),
                "signature and signature_header need secret, the [secrets] entry that signs requests"
            );
            return Ok(Self { signed: None });
        };
        let signature = match (signature, signature_header) {
            (None, _) => {
                bail!("secret needs signature: forgejo, github or hmac-sha256")
            }
            (Some(Scheme::HmacSha256), Some(header)) => {
                ensure!(
                    !header.is_empty()
                        && (header.bytes())
                            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
                    "signature_header {header:?} is not a header name"
                );
                Signature::Header(header.to_ascii_lowercase())
            }
            (Some(Scheme::HmacSha256), None) => {
                bail!(
                    "signature = \"hmac-sha256\" needs signature_header, the header that carries it"
                )
            }
            (Some(_), Some(_)) => bail!("signature_header applies to signature = \"hmac-sha256\""),
            (Some(Scheme::Forgejo), None) => Signature::Forgejo,
            (Some(Scheme::Github), None) => Signature::Github,
        };
        Ok(Self {
            signed: Some(Signed { secret, signature }),
        })
    }

    /// Its `secret`, `signature` and `signature_header` as config writes
    /// them.
    pub fn written(&self) -> (Option<String>, Option<Scheme>, Option<String>) {
        let Some(signed) = &self.signed else {
            return (None, None, None);
        };
        let (scheme, header) = match &signed.signature {
            Signature::Forgejo => (Scheme::Forgejo, None),
            Signature::Github => (Scheme::Github, None),
            Signature::Header(header) => (Scheme::HmacSha256, Some(header.clone())),
        };
        (Some(signed.secret.clone()), Some(scheme), header)
    }
}

/// The input of the firing of the trigger `name` that `request`, received
/// at `at`, makes: `payload`, its JSON body or form fields, `body`, its
/// text, `headers` and `query`, with `trigger` and `received_at`.
pub fn input(name: &str, request: &WebhookRequest, at: UnixTime) -> Input {
    let headers = table(
        request
            .headers
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.clone())),
    );
    let mut document = json!({
        "trigger": name,
        "received_at": local_time(at),
        "headers": headers,
        "query": table(request.query.iter().cloned()),
        "body": request.body,
    });
    if let Some(payload) = payload(request) {
        document["payload"] = payload;
    }
    let default_key = DELIVERY_HEADERS
        .iter()
        .find_map(|name| request.header(name))
        .map(str::to_owned);
    Input {
        document,
        default_key,
        request: Some(Request::new(REQUEST_ROOTS, LISTINGS)),
    }
}

/// `at` as an RFC 3339 time in the local time zone.
fn local_time(at: UnixTime) -> String {
    let timestamp = Timestamp::from_second(at.seconds()).unwrap_or(Timestamp::UNIX_EPOCH);
    (timestamp.display_with_offset(TimeZone::system().to_offset(timestamp))).to_string()
}

/// The fields of a form-encoded body, else the JSON body, else none.
fn payload(request: &WebhookRequest) -> Option<Value> {
    let form = (request.header("content-type"))
        .is_some_and(|kind| kind.starts_with("application/x-www-form-urlencoded"));
    if form {
        let fields = form_urlencoded::parse(request.body.as_bytes())
            .map(|(name, value)| (name.into_owned(), value.into_owned()));
        return Some(table(fields));
    }
    serde_json::from_str(&request.body).ok()
}

/// A JSON object of `pairs`, the later of a repeated name's values joined
/// to the earlier with `, `, as HTTP joins a repeated header.
fn table(pairs: impl Iterator<Item = (String, String)>) -> Value {
    let mut table = Map::new();
    for (name, value) in pairs {
        match table.get_mut(&name) {
            Some(Value::String(earlier)) => {
                earlier.push_str(", ");
                earlier.push_str(&value);
            }
            _ => {
                table.insert(name, Value::String(value));
            }
        }
    }
    Value::Object(table)
}

#[cfg(test)]
mod tests;
