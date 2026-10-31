use super::*;

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
