use super::*;
use crate::account::{AgentLogins, Launch};
use crate::test_support::install_test_script;

/// Finds the CLIs only in one directory.
struct Bin(PathBuf);

impl Launch for Bin {
    fn apply(&self, command: &mut Command) {
        command.env("PATH", &self.0);
    }
}

#[tokio::test]
async fn repeated_reads_send_the_validators_and_keep_the_unchanged_body() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    // Like gh, a 304 prints its head and exits with an error.
    let gh = dir.path().join("gh");
    install_test_script(
        &gh,
        &format!(
            r#"#!/bin/sh
echo "$*" >> '{log}'
case "$*" in
  *"If-None-Match: \"v1\""*) printf 'HTTP/2.0 304 Not Modified\n\n'; exit 1;;
esac
printf 'HTTP/2.0 200 OK\nETag: "v1"\n\n{{"number":7}}'
"#,
            log = log.display()
        ),
    );
    let agent = || {
        let logins = AgentLogins {
            launch: Box::new(Bin(dir.path().to_owned())),
            fj_home: None,
            fj: false,
            tea: false,
        };
        Account::agent(logins, &Default::default())
    };
    let memory = crate::transport::memory::Memory::default();
    let account = || agent().remembering(memory.clone());
    let repo = ForgeRepo::parse("https://github.com/team/project").unwrap();
    let read = Request::get("repos/team/project/issues/7".into());
    for _ in 0..2 {
        let body = repo.send(&account(), &read).await.unwrap();
        assert_eq!(body, serde_json::json!({"number": 7}));
    }
    let log = std::fs::read_to_string(log).unwrap();
    let lines: Vec<_> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(!lines[0].contains("If-None-Match"), "{log}");
    assert!(lines[1].ends_with("-H If-None-Match: \"v1\""), "{log}");
    // A change is not conditional, and an account without the memory
    // reads in full.
    let edit = Request::new(HttpMethod::Patch, read.endpoint.clone(), None);
    repo.send(&account(), &edit).await.unwrap();
    repo.send(&agent(), &read).await.unwrap();
    let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
    assert!(
        log.lines()
            .skip(2)
            .all(|line| !line.contains("If-None-Match")),
        "{log}"
    );
}

#[test]
fn forgejo_responses_report_status_and_messages() {
    assert_eq!(curl_body("{\"number\":7}\n201").unwrap(), "{\"number\":7}");
    assert_eq!(curl_body("\n204").unwrap(), "");
    let error = curl_body("{\"message\":\"user does not exist\"}\n404").unwrap_err();
    assert_eq!(format!("{error:#}"), "HTTP 404: user does not exist");
    let error = curl_body("<html>busy</html>\n502").unwrap_err();
    assert_eq!(format!("{error:#}"), "HTTP 502: <html>busy</html>");
    assert_eq!(format!("{:#}", curl_body("\n404").unwrap_err()), "HTTP 404");
    assert!(curl_body("{}").is_err());
}

#[test]
fn only_a_missing_curl_asks_for_curl() {
    use std::io::{Error, ErrorKind};
    let missing = curl_unrun(Error::from(ErrorKind::NotFound), 4);
    assert!(format!("{missing:#}").starts_with("curl is required"));
    let oversized = curl_unrun(Error::from(ErrorKind::FileTooLarge), MAX_RESPONSE_BYTES);
    assert_eq!(
        format!("{oversized:#}"),
        "the response is larger than 64 MiB; request fewer items, such as with a smaller page"
    );
    let broken = curl_unrun(Error::other("broken pipe"), 4);
    assert_eq!(format!("{broken:#}"), "read curl's response: broken pipe");
    let slow = curl_unrun(Error::from(ErrorKind::TimedOut), 4);
    assert!(slow.downcast_ref::<Transient>().is_some());
}

#[test]
fn encoding_keeps_unreserved_characters() {
    assert_eq!(encode("feat/a b#1", true), "feat/a%20b%231");
    assert_eq!(encode("feat/ü", false), "feat%2F%C3%BC");
}

#[test]
fn github_errors_name_each_validation_failure() {
    let body = r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists"},"base invalid"]}"#;
    assert_eq!(
        error_message(body).unwrap(),
        "Validation Failed: A pull request already exists; base invalid"
    );
    assert_eq!(error_message("not json"), None);
    let gitlab = r#"{"message":{"title":["can't be blank","is too short"],"base":"bad"}}"#;
    assert_eq!(
        error_message(gitlab).unwrap(),
        "base: \"bad\"; title can't be blank, is too short"
    );
    assert_eq!(
        error_message(r#"{"error":"insufficient_scope"}"#).unwrap(),
        "insufficient_scope"
    );
    assert_eq!(
        error_message(r#"{"type":"error","error":{"message":"Repository not found"}}"#).unwrap(),
        "Repository not found"
    );
}

#[tokio::test]
async fn forms_carry_the_file_under_its_field_and_a_plain_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a\"b.png");
    std::fs::write(&path, "png").unwrap();
    let upload = Upload {
        path,
        content_type: "image/png",
        field: Some("file"),
    };
    let (content_type, form) = multipart(&upload).await.unwrap();
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap();
    assert_eq!(
        String::from_utf8(form).unwrap(),
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"ab.png\"\r\n\
                 Content-Type: image/png\r\n\r\npng\r\n--{boundary}--\r\n"
        )
    );
}

#[test]
fn fj_tokens_are_read_per_host_and_refused_when_malformed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.json");
    std::fs::write(
        &path,
        r#"{"hosts":{"forge.example":{"type":"Application","token":"abc123"},
                "bad.example":{"type":"Application","token":"a\"b"}},"aliases":{}}"#,
    )
    .unwrap();
    assert_eq!(fj_token(&path, "forge.example").unwrap(), "abc123");
    assert!(format!("{:#}", fj_token(&path, "other.example").unwrap_err()).contains("no login"));
    assert!(fj_token(&path, "bad.example").is_err());
    assert!(fj_token(&dir.path().join("missing"), "forge.example").is_err());
}
