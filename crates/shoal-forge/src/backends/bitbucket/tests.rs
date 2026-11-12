use serde_json::json;

use super::*;

#[test]
fn pages_end_without_a_next_link() {
    let linked = json!({"values": [1], "next": "https://api.bitbucket.org/2.0/x?page=2"});
    assert_eq!(
        Bitbucket.page(&linked, 50).unwrap(),
        (&[1.into()][..], true)
    );
    assert!(!Bitbucket.page(&json!({"values": [1]}), 1).unwrap().1);
    assert!(Bitbucket.page(&json!([1, 2]), 2).is_none());
}

#[test]
fn tokens_are_bearer_or_basic_with_an_email() {
    assert_eq!(Bitbucket.authorization("abc", None), "Bearer abc");
    assert_eq!(
        Bitbucket.authorization("abc", Some("me@example.com")),
        format!("Basic {}", BASE64.encode("me@example.com:abc"))
    );
}

#[tokio::test]
async fn only_bitbucket_org_reaches_the_cloud_api() {
    let other = ForgeRepo::host(
        "bitbucket.example",
        "https",
        crate::backends::ForgeKind::Bitbucket,
    );
    let request = Request::get("user".into());
    let sent = Bitbucket.send(&other, &Account::default(), &request).await;
    assert!(sent.is_err());
}
