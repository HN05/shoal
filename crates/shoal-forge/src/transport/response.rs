//! A forge's HTTP response as a transport reads it: the status, the headers
//! that precede the body, and the body as text or, for a file, as bytes.
use anyhow::{Context, Result};

use crate::transport::api::http_body;

/// A response's status and its headers, with names in lowercase.
type Head = (u16, Vec<(String, String)>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Response {
    pub status: u16,
    /// Names in lowercase, in the order received.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    /// The value of the header `name`, given in lowercase.
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    /// The body of a successful response, or the error a failed one reports.
    pub fn success_body(&self) -> Result<&str> {
        http_body(self.status, &self.body)
    }

    /// A response printed with its head first, as `gh api --include` does;
    /// interim `1xx` heads before the final one are skipped.
    pub fn included(text: &str) -> Result<Self> {
        let ((status, headers), start) = included_head(text.as_bytes())?;
        Ok(Self {
            status,
            headers,
            body: text[start..].to_owned(),
        })
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

impl Fetched {
    /// A response printed with its head first, as `gh api --include` does.
    pub fn included(mut output: Vec<u8>) -> Result<Self> {
        let ((status, headers), start) = included_head(&output)?;
        output.drain(..start);
        Ok(Self {
            status,
            headers,
            body: output,
        })
    }

    /// The value of the header `name`, given in lowercase.
    pub fn header(&self, name: &str) -> Option<&str> {
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

/// The status and headers of the head `output` prints before its body, and
/// where the body starts; interim `1xx` heads before the final one are
/// skipped.
fn included_head(output: &[u8]) -> Result<(Head, usize)> {
    let mut start = 0;
    loop {
        let rest = &output[start..];
        let (head_end, body_start) = split_head(rest).context("the response has no headers")?;
        let head = String::from_utf8_lossy(&rest[..head_end]);
        let (status, headers) = parse_head(&head).context("the response has no HTTP status")?;
        if (100..200).contains(&status) && rest[body_start..].starts_with(b"HTTP/") {
            start += body_start;
            continue;
        }
        return Ok(((status, headers), start + body_start));
    }
}

/// The headers of the last response head in `text`, which a tool writes
/// apart from the body, such as curl's `--dump-header` or tea's `--include`
/// on standard error; none when it has no head.
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

/// Where the first blank line in `bytes` starts and where it ends.
fn split_head(bytes: &[u8]) -> Option<(usize, usize)> {
    [&b"\r\n\r\n"[..], b"\n\n"]
        .into_iter()
        .filter_map(|blank| {
            let at = bytes
                .windows(blank.len())
                .position(|window| window == blank)?;
            Some((at, at + blank.len()))
        })
        .min()
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
