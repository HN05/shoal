use super::*;

#[test]
fn media_types_follow_the_leading_bytes_as_go_detects_them() {
    let mut mp4 = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isommp42".to_vec();
    mp4.extend([0; 8]);
    for (data, media) in [
        (&b"\x89PNG\x0D\x0A\x1A\x0A rest"[..], "image/png"),
        (b"a\tb\n1\t2\n", "text/plain"),
        (b"\n  <!doctype html>", "text/html"),
        (b"<a>", "text/html"),
        (b"<abbr>", "text/plain"),
        (b" <?xml version", "text/xml"),
        (b"\x1F\x8B\x08\x00", "application/x-gzip"),
        (b"PK\x03\x04", "application/zip"),
        (&mp4, "video/mp4"),
        (
            b"\x00\x00\x00\x14ftypqt  \x00\x00\x00\x00qt  ",
            "application/octet-stream",
        ),
        (b"data\x00", "application/octet-stream"),
        (b"", "text/plain"),
    ] {
        assert_eq!(media_type(data), media, "{data:?}");
    }
    // Binary bytes past what Go reads do not count.
    let mut long = vec![b'a'; SNIFF_LEN];
    long.push(0);
    assert_eq!(media_type(&long), "text/plain");
}
