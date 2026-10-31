//! A forge's HTTP response as a transport reads it: the status, the headers
//! that precede the body, and the body as text or, for a file, as bytes.
use anyhow::Result;

use crate::transport::api::http_body;

/// A response's status and its headers, with names in lowercase.
type Head = (u16, Vec<(String, String)>);

/// The status and headers of a response, whatever its body.
pub(crate) trait Headed {
    fn status(&self) -> u16;
    /// The value of the header `name`, given in lowercase.
    fn header(&self, name: &str) -> Option<&str>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Response {
    pub status: u16,
    /// Names in lowercase, in the order received.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Headed for Response {
    fn status(&self) -> u16 {
        self.status
    }

    fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

impl Response {
    /// The body of a successful response, or the error a failed one reports.
    pub fn success_body(&self) -> Result<&str> {
        http_body(self.status, &self.body)
    }
}

/// A response whose body is a file, kept as bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fetched {
    pub status: u16,
    /// Names in lowercase, in the order received.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Headed for Fetched {
    fn status(&self) -> u16 {
        self.status
    }

    fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

/// The value of the header `name`, given in lowercase, among `headers`.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header, _)| header == name)
        .map(|(_, value)| value.as_str())
}

/// The headers of the last response head in `text`, which curl's
/// `--dump-header` writes apart from the body; none when it has no head.
pub(crate) fn last_head(text: &str) -> Vec<(String, String)> {
    let start = text
        .match_indices("HTTP/")
        .map(|(at, _)| at)
        .filter(|&at| at == 0 || text[..at].ends_with('\n'))
        .last();
    start
        .and_then(|at| parse_head(&text[at..]))
        .map(|(_, headers)| headers)
        .unwrap_or_default()
}

/// The status and headers of a head, which ends at its first blank line.
fn parse_head(head: &str) -> Option<Head> {
    let mut lines = head.lines().map(|line| line.trim_end_matches('\r'));
    let status = lines
        .next()?
        .strip_prefix("HTTP/")?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let headers = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Some((status, headers))
}

#[cfg(test)]
mod tests;
