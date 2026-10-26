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
    let text =
        "HTTP/1.1 100 Continue\n\nHTTP/1.1 429 Too Many\nRetry-After: 30\n\ncurl: (22) failed\n";
    assert_eq!(
        last_head(text),
        [("retry-after".to_owned(), "30".to_owned())]
    );
    assert!(last_head("curl: (6) Could not resolve host").is_empty());
}
