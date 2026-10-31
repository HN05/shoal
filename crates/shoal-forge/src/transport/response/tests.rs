use super::*;

#[test]
fn the_heads_before_the_body_give_the_last_ones_headers() {
    let output = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 429 Too Many\r\nRetry-After: 30\r\n\r\n{\"message\":\"HTTP/1.1 200\"}\n429";
    let (headers, rest) = split_heads(output);
    assert_eq!(headers, [("retry-after".to_owned(), "30".to_owned())]);
    assert_eq!(rest, b"{\"message\":\"HTTP/1.1 200\"}\n429");
    let (headers, rest) = split_heads(b"HTTP/2 204\n\n\n204");
    assert!(headers.is_empty());
    assert_eq!(rest, b"\n204");
    let (headers, rest) = split_heads(b"plain\n200");
    assert!(headers.is_empty());
    assert_eq!(rest, b"plain\n200");
}
