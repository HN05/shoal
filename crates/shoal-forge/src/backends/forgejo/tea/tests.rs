use super::*;

#[test]
fn logins_match_the_host_and_prefer_its_port() {
    let text = r#"[
            {"name": "other", "url": "https://other.example", "user": "a", "default": "true"},
            {"name": "web", "url": "https://gitea.example:3000/", "user": "a", "default": "false"},
            {"name": "sub", "url": "https://sub.example/gitea", "user": "a", "default": "false"},
            {"name": "plain", "url": "http://plain.example:3000", "user": "a", "default": "false"},
            {"name": "plain-other", "url": "http://plain.example:4000", "user": "a", "default": "false"}
        ]"#;
    assert_eq!(
        login_for(text, "gitea.example").unwrap(),
        Some(TeaLogin {
            name: "web".into(),
            url: "https://gitea.example:3000".into()
        })
    );
    assert_eq!(
        login_for(text, "gitea.example:3000").unwrap().unwrap().name,
        "web"
    );
    assert_eq!(
        login_for(text, "plain.example:4000").unwrap().unwrap().name,
        "plain-other"
    );
    assert_eq!(login_for(text, "gitea.example:4000").unwrap(), None);
    assert_eq!(login_for(text, "sub.example").unwrap(), None);
    assert_eq!(login_for("[]", "gitea.example").unwrap(), None);
    assert!(login_for("not json", "gitea.example").is_err());
}

#[test]
fn status_comes_from_the_included_response_line() {
    assert_eq!(
        status("HTTP/2.0 404 Not Found\nContent-Type: json\n").unwrap(),
        404
    );
    assert_eq!(status("note\nHTTP/1.1 200 OK\n").unwrap(), 200);
    assert!(status("").is_err());
    assert!(status("HTTP/1.1 OK").is_err());
}
