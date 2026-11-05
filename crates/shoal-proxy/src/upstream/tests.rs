use super::*;

#[test]
fn requests_keep_their_path_after_the_upstream_prefix() {
    let plain = Upstream::parse("http://127.0.0.1:8317").unwrap();
    assert_eq!(plain.authority(), "127.0.0.1:8317");
    assert_eq!(
        plain.join("/v1/messages?beta=true").unwrap(),
        "http://127.0.0.1:8317/v1/messages?beta=true"
    );
    let prefixed = Upstream::parse("https://chatgpt.com/backend-api/codex/").unwrap();
    assert_eq!(
        prefixed.join("/responses").unwrap(),
        "https://chatgpt.com/backend-api/codex/responses"
    );
    for url in ["ftp://host", "127.0.0.1:8317", "http://host/?a=b"] {
        assert!(Upstream::parse(url).is_err(), "{url}");
    }
}
