use super::*;

#[test]
fn curl_output_splits_at_the_head_size_curl_reports() {
    let heads =
        "HTTP/1.1 302 Found\r\nLocation: /file\r\n\r\nHTTP/1.1 200 OK\r\nETag: \"a\"\r\n\r\n";
    // A file that itself starts with a response head stays whole.
    let file = b"HTTP/1.1 200 OK\r\n\r\npayload\n\0\xff";
    let output = [
        heads.as_bytes(),
        file,
        format!("\n{} 200", heads.len()).as_bytes(),
    ]
    .concat();
    let split = split_output(&output).unwrap();
    assert_eq!(split.status, 200);
    assert_eq!(split.headers, [("etag".to_owned(), "\"a\"".to_owned())]);
    assert_eq!(&output[split.body], file);
}

#[test]
fn curl_output_without_a_head_or_body_still_has_a_status() {
    let split = split_output(b"\n0 404").unwrap();
    assert_eq!(split.status, 404);
    assert!(split.headers.is_empty());
    assert!(split.body.is_empty());
    assert!(split_output(b"no status").is_err());
    assert!(split_output(b"\n200").is_err());
    assert!(split_output(b"short\n99 200").is_err());
}
