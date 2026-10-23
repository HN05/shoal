//! A forge's HTTP response as a transport reads it: the status, the headers
//! that precede the body, and the body text.
use anyhow::{Context, Result};

use crate::transport::api::http_body;

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
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    /// The body of a successful response, or the error a failed one reports.
    pub fn success_body(&self) -> Result<&str> {
        http_body(self.status, &self.body)
    }

    /// A response printed with its head first, as `gh api --include` does;
    /// interim `1xx` heads before the final one are skipped.
    pub fn included(text: &str) -> Result<Self> {
        let mut rest = text;
        loop {
            let (head, body) = split_head(rest).context("the response has no headers")?;
            let (status, headers) = parse_head(head).context("the response has no HTTP status")?;
            if (100..200).contains(&status) && body.starts_with("HTTP/") {
                rest = body;
                continue;
            }
            return Ok(Self {
                status,
                headers,
                body: body.to_owned(),
            });
        }
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

/// The head and body of `text`, split at the first blank line.
fn split_head(text: &str) -> Option<(&str, &str)> {
    ["\r\n\r\n", "\n\n"]
        .into_iter()
        .filter_map(|blank| text.find(blank).map(|at| (at, blank.len())))
        .min()
        .map(|(at, len)| (&text[..at], &text[at + len..]))
}

/// The status and headers of a head, which ends at its first blank line.
fn parse_head(head: &str) -> Option<(u16, Vec<(String, String)>)> {
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
mod tests {
    use super::*;

    #[test]
    fn included_responses_skip_interim_heads_and_keep_the_body() {
        let text = "HTTP/1.1 100 Continue\r\n\r\nHTTP/2.0 304 Not Modified\r\nEtag: \"a\"\r\n\
                    X-Ratelimit-Remaining: 7\r\n\r\n";
        let response = Response::included(text).unwrap();
        assert_eq!(response.status, 304);
        assert_eq!(
            response.headers,
            [
                ("etag".to_owned(), "\"a\"".to_owned()),
                ("x-ratelimit-remaining".to_owned(), "7".to_owned())
            ]
        );
        assert_eq!(response.body, "");
        let response = Response::included("HTTP/2.0 200 OK\nA: b\n\n{\"n\":1}\n\nmore").unwrap();
        assert_eq!(response.body, "{\"n\":1}\n\nmore");
        assert!(Response::included("{\"n\":1}").is_err());
    }

    #[test]
    fn the_last_head_is_read_from_text_around_it() {
        let text = "HTTP/1.1 100 Continue\n\nHTTP/1.1 429 Too Many\nRetry-After: 30\n\ncurl: (22) failed\n";
        assert_eq!(
            last_head(text),
            [("retry-after".to_owned(), "30".to_owned())]
        );
        assert!(last_head("curl: (6) Could not resolve host").is_empty());
    }
}
